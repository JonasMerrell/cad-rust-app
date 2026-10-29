//! The circuit solver: modified nodal analysis with Newton-Raphson iterations for the DC
//! operating point, a second-order Gear (BDF2) transient with adaptive steps and events (a 555's flip-flop),
//! and a complex small-signal AC sweep about the operating point.
//!
//! Node 0 is ground. The unknowns are the other node voltages and then one current for each
//! voltage source and inductor. Units are SI (V, A, Ω, F, H, s).

use std::ops::{Add, Div, Mul, Neg, Sub};

/// Thermal voltage at 27 °C.
pub const VT: f64 = 0.025852;

// ---------------------------------------------------------------- source waveforms

#[derive(Clone, Debug, PartialEq)]
pub enum Wave {
    Dc(f64),
    /// SPICE PULSE: v1, v2, delay, rise, fall, width, period (0: one pulse).
    Pulse {
        v1: f64,
        v2: f64,
        td: f64,
        tr: f64,
        tf: f64,
        pw: f64,
        per: f64,
    },
    /// SPICE SIN: offset, amplitude, frequency, delay, damping.
    Sin {
        vo: f64,
        va: f64,
        f: f64,
        td: f64,
        theta: f64,
    },
    /// Piecewise linear (time, value) corners.
    Pwl(Vec<(f64, f64)>),
}

impl Wave {
    pub fn at(&self, t: f64) -> f64 {
        match self {
            Wave::Dc(v) => *v,
            Wave::Pulse { v1, v2, td, tr, tf, pw, per } => {
                if t < *td {
                    return *v1;
                }
                let mut tt = t - td;
                if *per > 0.0 {
                    tt %= per;
                }
                if tt < *tr {
                    v1 + (v2 - v1) * tt / tr
                } else if tt < tr + pw {
                    *v2
                } else if tt < tr + pw + tf {
                    v2 + (v1 - v2) * (tt - tr - pw) / tf
                } else {
                    *v1
                }
            }
            Wave::Sin { vo, va, f, td, theta } => {
                if t < *td {
                    *vo
                } else {
                    let s = t - td;
                    vo + va * (2.0 * std::f64::consts::PI * f * s).sin() * (-theta * s).exp()
                }
            }
            Wave::Pwl(pts) => {
                let Some(first) = pts.first() else { return 0.0 };
                if t <= first.0 {
                    return first.1;
                }
                for w in pts.windows(2) {
                    let ((t0, v0), (t1, v1)) = (w[0], w[1]);
                    if t <= t1 {
                        return if t1 > t0 { v0 + (v1 - v0) * (t - t0) / (t1 - t0) } else { v1 };
                    }
                }
                pts.last().unwrap().1
            }
        }
    }

    /// The next corner of the waveform after `t` (infinity when there is none).
    pub fn next_break(&self, t: f64) -> f64 {
        let eps = 1e-15 + t.abs() * 1e-12;
        match self {
            Wave::Dc(_) => f64::INFINITY,
            Wave::Pulse { td, tr, tf, pw, per, .. } => {
                let corners = [0.0, *tr, tr + pw, tr + pw + tf];
                if t + eps < *td {
                    return *td;
                }
                let k0 = if *per > 0.0 { ((t - td) / per).floor().max(0.0) } else { 0.0 };
                for k in [k0, k0 + 1.0] {
                    if k > 0.0 && *per <= 0.0 {
                        break;
                    }
                    for c in corners {
                        let b = td + k * per + c;
                        if b > t + eps {
                            return b;
                        }
                    }
                }
                f64::INFINITY
            }
            Wave::Sin { td, .. } => {
                if t + eps < *td {
                    *td
                } else {
                    f64::INFINITY
                }
            }
            Wave::Pwl(pts) => pts.iter().map(|p| p.0).find(|b| *b > t + eps).unwrap_or(f64::INFINITY),
        }
    }

    /// The longest step that still follows the waveform's shape.
    pub fn max_step(&self) -> f64 {
        match self {
            Wave::Sin { f, .. } if *f > 0.0 => 1.0 / (40.0 * f),
            Wave::Pulse { pw, per, .. } => {
                // the edges are breakpoints; this keeps the flat parts from being skipped
                let mut m = if pw.is_finite() && *pw > 0.0 { pw / 2.0 } else { f64::INFINITY };
                if *per > 0.0 {
                    m = m.min(per / 20.0);
                }
                m
            }
            _ => f64::INFINITY,
        }
    }

    /// The largest magnitude the waveform reaches.
    pub fn peak(&self) -> f64 {
        match self {
            Wave::Dc(v) => v.abs(),
            Wave::Pulse { v1, v2, .. } => v1.abs().max(v2.abs()),
            Wave::Sin { vo, va, .. } => vo.abs() + va.abs(),
            Wave::Pwl(p) => p.iter().map(|x| x.1.abs()).fold(0.0, f64::max),
        }
    }
}

// ---------------------------------------------------------------- device models

#[derive(Clone, Debug, PartialEq)]
pub struct Diode {
    pub is: f64,
    pub n: f64,
    /// Reverse breakdown voltage (infinite: none).
    pub bv: f64,
    /// Current at the breakdown voltage.
    pub ibv: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bjt {
    pub is: f64,
    pub bf: f64,
    pub br: f64,
    /// Forward Early voltage (infinite: none).
    pub vaf: f64,
    pub nf: f64,
}

/// A level 1 (square law) MOSFET.
#[derive(Clone, Debug, PartialEq)]
pub struct Mos {
    /// Threshold voltage (positive for both channel types; the sign is the channel's).
    pub vto: f64,
    /// Transconductance parameter, A/V².
    pub kp: f64,
    pub lambda: f64,
}

/// An op amp: a current-limited transconductance stage into a compensation capacitor (giving the
/// gain-bandwidth and slew rate), an output that stays inside its rails, and an output resistance.
#[derive(Clone, Debug, PartialEq)]
pub struct OpAmp {
    pub a0: f64,
    pub gbw: f64,
    /// Slew rate, V/s.
    pub sr: f64,
    /// How close the output gets to the positive and the negative rail.
    pub hr_hi: f64,
    pub hr_lo: f64,
    pub ro: f64,
}

impl OpAmp {
    const RP: f64 = 1e5;
    /// The compensation capacitor on the internal node.
    pub fn cint(&self) -> f64 {
        (self.a0 / Self::RP) / (2.0 * std::f64::consts::PI * self.gbw)
    }
}

/// A linear regulator: output `vset` above its reference pin, at most the input less `vdo`.
#[derive(Clone, Debug, PartialEq)]
pub struct Reg {
    pub vset: f64,
    pub vdo: f64,
    /// Ground pin current (negative: current out of an adjust pin).
    pub iq: f64,
    pub ro: f64,
}

/// A 555 timer's comparators, flip-flop, output stage and discharge transistor.
#[derive(Clone, Debug, PartialEq)]
pub struct Timer {
    pub cmos: bool,
}

impl Timer {
    fn out_r(&self) -> f64 {
        if self.cmos { 40.0 } else { 10.0 }
    }
    /// Output high is this far below VCC.
    fn drop(&self) -> f64 {
        if self.cmos { 0.0 } else { 1.7 }
    }
    fn low(&self) -> f64 {
        if self.cmos { 0.0 } else { 0.1 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dev {
    /// [a, b] conductance.
    R(f64),
    /// [a, b] capacitance.
    C(f64),
    /// [plus, minus]: current out of the plus terminal into the circuit; AC magnitude.
    I(Wave, f64),
    /// [anode, cathode].
    D(Diode),
    /// [c, b, e]; PNP when set.
    Q(Bjt, bool),
    /// [d, g, s]; P-channel when set.
    M(Mos, bool),
    /// [in+, in-, out, v+, v-, internal].
    Op(OpAmp),
    /// [in, ref, out].
    Reg(Reg),
    /// [gnd, trig, out, reset, ctrl, thr, dis, vcc].
    T555(Timer),
    /// [plus, minus] voltage source with its current unknown; AC magnitude.
    V(Wave, f64),
    /// [a, b] inductance and series resistance, with its current unknown.
    L(f64, f64),
}

impl Dev {
    pub fn branch(&self) -> bool {
        matches!(self, Dev::V(..) | Dev::L(..))
    }
}

#[derive(Clone, Debug)]
pub struct Elem {
    pub dev: Dev,
    pub n: Vec<usize>,
    /// Index of its current among the unknowns' branch part (branch elements only).
    pub br: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Circuit {
    /// Node names; 0 is ground.
    pub nodes: Vec<String>,
    pub elems: Vec<Elem>,
    pub nbr: usize,
}

impl Circuit {
    pub fn new() -> Circuit {
        Circuit { nodes: vec!["0".into()], elems: vec![], nbr: 0 }
    }
    pub fn node(&mut self, name: impl Into<String>) -> usize {
        self.nodes.push(name.into());
        self.nodes.len() - 1
    }
    pub fn add(&mut self, dev: Dev, n: &[usize]) -> usize {
        let br = if dev.branch() {
            self.nbr += 1;
            self.nbr - 1
        } else {
            usize::MAX
        };
        self.elems.push(Elem { dev, n: n.to_vec(), br });
        self.elems.len() - 1
    }
    /// Number of unknowns.
    pub fn size(&self) -> usize {
        self.nodes.len() - 1 + self.nbr
    }
}

// ---------------------------------------------------------------- small numerics

fn sexp(x: f64) -> f64 {
    // exp that grows linearly past 80 so Newton steps stay finite
    if x > 80.0 { 80f64.exp() * (1.0 + x - 80.0) } else { x.exp() }
}

fn dsexp(x: f64) -> f64 {
    if x > 80.0 { 80f64.exp() } else { x.exp() }
}

/// SPICE's junction voltage limiting.
fn pnjlim(vnew: f64, vold: f64, vt: f64, vcrit: f64) -> f64 {
    if vnew > vcrit && (vnew - vold).abs() > 2.0 * vt {
        if vold > 0.0 {
            let arg = 1.0 + (vnew - vold) / vt;
            if arg > 0.0 { vold + vt * arg.ln() } else { vcrit }
        } else {
            vt * (vnew / vt).max(1e-30).ln()
        }
    } else {
        vnew
    }
}

fn vcrit(is: f64, vt: f64) -> f64 {
    vt * (vt / (std::f64::consts::SQRT_2 * is)).ln()
}

fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// Smooth max(x, 0) with width s, and its slope.
fn softplus(x: f64, s: f64) -> (f64, f64) {
    let u = x / s;
    let v = if u > 30.0 { x } else { s * u.exp().ln_1p() };
    (v, sigmoid(u))
}

/// Smooth min(a, b) and its slopes.
fn smin(a: f64, b: f64, k: f64) -> (f64, f64, f64) {
    let r = ((a - b) * (a - b) + k * k).sqrt();
    ((a + b - r) / 2.0, (1.0 - (a - b) / r) / 2.0, (1.0 + (a - b) / r) / 2.0)
}

/// Smooth max(a, 0) and its slope.
fn smax0(a: f64, k: f64) -> (f64, f64) {
    let r = (a * a + k * k).sqrt();
    ((a + r) / 2.0, (1.0 + a / r) / 2.0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cx {
    pub re: f64,
    pub im: f64,
}

impl Cx {
    pub fn new(re: f64, im: f64) -> Cx {
        Cx { re, im }
    }
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
}

impl Add for Cx {
    type Output = Cx;
    fn add(self, o: Cx) -> Cx {
        Cx::new(self.re + o.re, self.im + o.im)
    }
}
impl Sub for Cx {
    type Output = Cx;
    fn sub(self, o: Cx) -> Cx {
        Cx::new(self.re - o.re, self.im - o.im)
    }
}
impl Mul for Cx {
    type Output = Cx;
    fn mul(self, o: Cx) -> Cx {
        Cx::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}
impl Div for Cx {
    type Output = Cx;
    fn div(self, o: Cx) -> Cx {
        let d = o.re * o.re + o.im * o.im;
        Cx::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
    }
}
impl Neg for Cx {
    type Output = Cx;
    fn neg(self) -> Cx {
        Cx::new(-self.re, -self.im)
    }
}

trait Num: Copy + Default + Add<Output = Self> + Sub<Output = Self> + Mul<Output = Self> + Div<Output = Self> {
    fn mag(self) -> f64;
}
impl Num for f64 {
    fn mag(self) -> f64 {
        self.abs()
    }
}
impl Num for Cx {
    fn mag(self) -> f64 {
        self.re.abs() + self.im.abs()
    }
}

/// Solve a·x = b in place (b becomes x) by LU with partial pivoting; false when singular.
fn lu_solve<T: Num>(n: usize, a: &mut [T], b: &mut [T]) -> bool {
    let scale = a.iter().map(|v| v.mag()).fold(0.0, f64::max).max(1e-300);
    for k in 0..n {
        let mut p = k;
        let mut best = a[k * n + k].mag();
        for r in k + 1..n {
            let m = a[r * n + k].mag();
            if m > best {
                best = m;
                p = r;
            }
        }
        if best <= scale * 1e-18 {
            return false;
        }
        if p != k {
            for c in 0..n {
                a.swap(k * n + c, p * n + c);
            }
            b.swap(k, p);
        }
        let piv = a[k * n + k];
        for r in k + 1..n {
            let f = a[r * n + k] / piv;
            if f.mag() == 0.0 {
                continue;
            }
            for c in k + 1..n {
                let v = a[k * n + c];
                a[r * n + c] = a[r * n + c] - f * v;
            }
            b[r] = b[r] - f * b[k];
        }
    }
    for k in (0..n).rev() {
        let mut s = b[k];
        for c in k + 1..n {
            s = s - a[k * n + c] * b[c];
        }
        b[k] = s / a[k * n + k];
    }
    true
}

// ---------------------------------------------------------------- device evaluation

/// A device linearised at terminal voltages `u`: currents into the device and their Jacobian.
struct Lin {
    u: Vec<f64>,
    i: Vec<f64>,
    /// Row-major k × k: d i[r] / d u[c].
    j: Vec<f64>,
    limited: bool,
}

impl Lin {
    fn new(u: Vec<f64>) -> Lin {
        let k = u.len();
        Lin { u, i: vec![0.0; k], j: vec![0.0; k * k], limited: false }
    }
    fn g(&mut self, r: usize, c: usize, v: f64) {
        let k = self.u.len();
        self.j[r * k + c] += v;
    }
}

fn diode_i(m: &Diode, v: f64) -> (f64, f64) {
    let nvt = m.n * VT;
    let e = sexp(v / nvt);
    let mut i = m.is * (e - 1.0);
    let mut g = m.is * dsexp(v / nvt) / nvt;
    if m.bv.is_finite() {
        let x = -(v + m.bv) / VT;
        i -= m.ibv * sexp(x);
        g += m.ibv * dsexp(x) / VT;
    }
    (i, g)
}

/// Junction memory per element: the last limited voltages.
#[derive(Clone, Copy, Debug, Default)]
struct Mem {
    a: f64,
    b: f64,
}

fn eval(dev: &Dev, v: &[f64], mem: &mut Mem, on: bool, limit: bool) -> Lin {
    match dev {
        Dev::D(m) => {
            let raw = v[0] - v[1];
            let mut vd = raw;
            if limit {
                let nvt = m.n * VT;
                vd = pnjlim(raw, mem.a, nvt, vcrit(m.is, nvt));
                if m.bv.is_finite() && raw < -m.bv + 1.0 {
                    // the breakdown side, mirrored
                    let r = -(raw + m.bv);
                    let o = -(mem.a + m.bv);
                    vd = -(pnjlim(r, o, VT, vcrit(m.ibv.max(1e-30), VT)) + m.bv);
                }
            }
            let limited = (vd - raw).abs() > 1e-12;
            mem.a = vd;
            let (i, g) = diode_i(m, vd);
            let mut l = Lin::new(vec![v[1] + vd, v[1]]);
            l.limited = limited;
            l.i = vec![i, -i];
            l.j = vec![g, -g, -g, g];
            l
        }
        Dev::Q(m, pnp) => {
            let s = if *pnp { -1.0 } else { 1.0 };
            let (vc, vb, ve) = (v[0], v[1], v[2]);
            let raw_be = s * (vb - ve);
            let raw_bc = s * (vb - vc);
            let (mut vbe, mut vbc) = (raw_be, raw_bc);
            if limit {
                vbe = pnjlim(raw_be, mem.a, m.nf * VT, vcrit(m.is, m.nf * VT));
                vbc = pnjlim(raw_bc, mem.b, VT, vcrit(m.is, VT));
            }
            let limited = (vbe - raw_be).abs() > 1e-12 || (vbc - raw_bc).abs() > 1e-12;
            mem.a = vbe;
            mem.b = vbc;
            let nvt = m.nf * VT;
            let ef = sexp(vbe / nvt);
            let er = sexp(vbc / VT);
            let ifw = m.is * (ef - 1.0);
            let irv = m.is * (er - 1.0);
            let gif = m.is * dsexp(vbe / nvt) / nvt;
            let gir = m.is * dsexp(vbc / VT) / VT;
            let (kq, dkq) = if m.vaf.is_finite() { ((1.0 - vbc / m.vaf).max(0.05), if 1.0 - vbc / m.vaf > 0.05 { -1.0 / m.vaf } else { 0.0 }) } else { (1.0, 0.0) };
            let ict = (ifw - irv) * kq;
            let ict_be = gif * kq;
            let ict_bc = -gir * kq + (ifw - irv) * dkq;
            let ib = ifw / m.bf + irv / m.br;
            let (ib_be, ib_bc) = (gif / m.bf, gir / m.br);
            let ic = ict - irv / m.br;
            let (ic_be, ic_bc) = (ict_be, ict_bc - gir / m.br);
            let ie = -(ic + ib);
            let (ie_be, ie_bc) = (-(ic_be + ib_be), -(ic_bc + ib_bc));
            let mut l = Lin::new(vec![vb - s * vbc, vb, vb - s * vbe]);
            l.limited = limited;
            l.i = vec![s * ic, s * ib, s * ie];
            for (r, (dbe, dbc)) in [(ic_be, ic_bc), (ib_be, ib_bc), (ie_be, ie_bc)].into_iter().enumerate() {
                l.g(r, 0, -dbc);
                l.g(r, 1, dbe + dbc);
                l.g(r, 2, -dbe);
            }
            l
        }
        Dev::M(m, pmos) => {
            let s = if *pmos { -1.0 } else { 1.0 };
            let (vd, vg, vs) = (v[0], v[1], v[2]);
            let raw_gs = s * (vg - vs);
            let vds = s * (vd - vs);
            let mut vgs = raw_gs;
            if limit {
                vgs = mem.a + (raw_gs - mem.a).clamp(-1.0, 1.0);
            }
            let limited = (vgs - raw_gs).abs() > 1e-12;
            mem.a = vgs;
            // square law with a smooth turn-on
            let f = |vgs: f64, vds: f64| -> (f64, f64, f64) {
                let (vov, sig) = softplus(vgs - m.vto, 0.02);
                let cl = 1.0 + m.lambda * vds;
                if vds < vov {
                    let base = vov * vds - vds * vds / 2.0;
                    (m.kp * base * cl, m.kp * vds * cl * sig, m.kp * (vov - vds) * cl + m.kp * base * m.lambda)
                } else {
                    (m.kp / 2.0 * vov * vov * cl, m.kp * vov * cl * sig, m.kp / 2.0 * vov * vov * m.lambda)
                }
            };
            let (id, gm, gds) = if vds >= 0.0 {
                f(vgs, vds)
            } else {
                let (i, f1, f2) = f(vgs - vds, -vds);
                (-i, -f1, f1 + f2)
            };
            let mut l = Lin::new(vec![vs + s * vds, vs + s * vgs, vs]);
            l.limited = limited;
            l.i = vec![s * id, 0.0, -s * id];
            l.g(0, 0, gds);
            l.g(0, 1, gm);
            l.g(0, 2, -(gm + gds));
            l.g(2, 0, -gds);
            l.g(2, 1, -gm);
            l.g(2, 2, gm + gds);
            l
        }
        Dev::Op(m) => {
            // [p, n, out, vp, vn, int]
            let (vd, vout, vint) = (v[0] - v[1], v[2], v[5]);
            let hi = v[3] - m.hr_hi;
            let lo = v[4] + m.hr_lo;
            let (h, dh) = if (hi - lo) / 2.0 > 0.05 { ((hi - lo) / 2.0, 0.5) } else { (0.05, 0.0) };
            let mid = (hi + lo) / 2.0;
            let gm = m.a0 / OpAmp::RP;
            let imax = m.sr * m.cint();
            let x = gm * vd / imax;
            let th = x.tanh();
            let it = imax * th;
            let dit = gm * (1.0 - th * th);
            let s = 0.05;
            let (pa, sa) = softplus(vint - 3.0 * h, s);
            let (pb, sb) = softplus(-vint - 3.0 * h, s);
            let gc = 1.0;
            let mut l = Lin::new(v.to_vec());
            // internal node
            l.i[5] = vint / OpAmp::RP - it + gc * (pa - pb);
            l.g(5, 5, 1.0 / OpAmp::RP + gc * (sa + sb));
            l.g(5, 0, -dit);
            l.g(5, 1, dit);
            let dh_c = gc * (-3.0 * sa + 3.0 * sb);
            l.g(5, 3, dh_c * dh);
            l.g(5, 4, -dh_c * dh);
            // output stage
            let w = vint / h;
            let tw = w.tanh();
            let vt = mid + h * tw;
            let dvt_int = 1.0 - tw * tw;
            let dvt_h = tw - w * (1.0 - tw * tw);
            let f = (vout - vt) / m.ro;
            let mut df = [0.0; 6];
            df[2] = 1.0 / m.ro;
            df[5] = -dvt_int / m.ro;
            df[3] = -(0.5 + dvt_h * dh) / m.ro;
            df[4] = -(0.5 - dvt_h * dh) / m.ro;
            // sunk current leaves by v-, sourced current comes from v+
            let e = 1e-9;
            let r = (f * f + e * e).sqrt();
            let pos = (f + r) / 2.0;
            let dpos = (1.0 + f / r) / 2.0;
            l.i[2] = f;
            l.i[4] = -pos;
            l.i[3] = -(f - pos);
            for c in 0..6 {
                l.g(2, c, df[c]);
                l.g(4, c, -dpos * df[c]);
                l.g(3, c, -(1.0 - dpos) * df[c]);
            }
            l
        }
        Dev::Reg(m) => {
            // [in, ref, out]
            let (vin, vref, vout) = (v[0], v[1], v[2]);
            let k = 0.05;
            let (a, da_set, da_in) = smin(m.vset, vin - vref - m.vdo, k);
            let (b, db) = smax0(a, k);
            let vt = vref + b;
            // d vt / d (vin, vref)
            let dvt_in = db * da_in;
            let dvt_ref = 1.0 - db * da_in;
            let _ = da_set;
            let f = (vout - vt) / m.ro;
            let df = [-dvt_in / m.ro, -dvt_ref / m.ro, 1.0 / m.ro];
            let e = 1e-9;
            let r = (f * f + e * e).sqrt();
            let pos = (f + r) / 2.0;
            let dpos = (1.0 + f / r) / 2.0;
            // quiescent current from in to ref, fading out without input
            let q = ((vin - vref) / 0.3).tanh();
            let iq = m.iq * q;
            let diq = m.iq * (1.0 - q * q) / 0.3;
            let mut l = Lin::new(v.to_vec());
            l.i[2] = f;
            l.i[1] = -pos - iq;
            l.i[0] = -(f - pos) + iq;
            for c in 0..3 {
                l.g(2, c, df[c]);
                l.g(1, c, -dpos * df[c]);
                l.g(0, c, -(1.0 - dpos) * df[c]);
            }
            l.g(0, 0, diq);
            l.g(0, 1, -diq);
            l.g(1, 0, -diq);
            l.g(1, 1, diq);
            l
        }
        Dev::T555(m) => {
            // [gnd, trig, out, reset, ctrl, thr, dis, vcc]
            let mut l = Lin::new(v.to_vec());
            let go = 1.0 / m.out_r();
            if on {
                let f = go * (v[7] - m.drop() - v[2]);
                l.i[7] += f;
                l.i[2] -= f;
                for (r, sg) in [(7, 1.0), (2, -1.0)] {
                    l.g(r, 7, sg * go);
                    l.g(r, 2, -sg * go);
                }
            } else {
                let f = go * (v[2] - v[0] - m.low());
                l.i[2] += f;
                l.i[0] -= f;
                for (r, sg) in [(2, 1.0), (0, -1.0)] {
                    l.g(r, 2, sg * go);
                    l.g(r, 0, -sg * go);
                }
            }
            let gd = if on { 1e-9 } else { 1.0 / 15.0 };
            let f = gd * (v[6] - v[0]);
            l.i[6] += f;
            l.i[0] -= f;
            for (r, sg) in [(6, 1.0), (0, -1.0)] {
                l.g(r, 6, sg * gd);
                l.g(r, 0, -sg * gd);
            }
            l
        }
        _ => Lin::new(v.to_vec()),
    }
}

/// The 555's flip-flop after its comparators see `v` (terminal voltages).
fn timer_next(v: &[f64], on: bool) -> bool {
    let (trig, reset, ctrl, thr) = (v[1] - v[0], v[3] - v[0], v[4] - v[0], v[5] - v[0]);
    if reset < 0.7 {
        false
    } else if trig < ctrl / 2.0 {
        true
    } else if thr > ctrl {
        false
    } else {
        on
    }
}

// ---------------------------------------------------------------- the engine

#[derive(Clone, Copy, Debug)]
enum Mode {
    Dc,
    /// `hp`: the step before this one (0: backward Euler, after a start, a breakpoint or an event).
    Tran {
        t: f64,
        dt: f64,
        hp: f64,
    },
}

/// Variable-step BDF2 weights: dx/dt ≈ a0 x(n+1) + a1 x(n) + a2 x(n-1).
fn gear(h: f64, hp: f64) -> (f64, f64, f64) {
    if hp <= 0.0 {
        return (1.0 / h, -1.0 / h, 0.0);
    }
    ((2.0 * h + hp) / (h * (h + hp)), -(h + hp) / (h * hp), h / (hp * (h + hp)))
}

/// A capacitor's voltage (and the one before) and current, or an inductor's current (and the one
/// before) and voltage, at the last accepted point.
#[derive(Clone, Copy, Debug, Default)]
struct Hist {
    v: f64,
    i: f64,
    v1: f64,
    i1: f64,
}

pub struct Tran {
    pub t: Vec<f64>,
    /// Unknowns at each time.
    pub x: Vec<Vec<f64>>,
    /// Terminal currents at each time (see `Sim::offsets`).
    pub i: Vec<Vec<f64>>,
}

pub struct Sim<'a> {
    pub c: &'a Circuit,
    n: usize,
    nn: usize,
    hist: Vec<Hist>,
    mem: Vec<Mem>,
    on: Vec<bool>,
    gmin: f64,
    scale: f64,
    pub notes: Vec<String>,
    /// Where each element's terminal currents start in a current vector.
    pub offsets: Vec<usize>,
}

impl<'a> Sim<'a> {
    pub fn new(c: &'a Circuit) -> Sim<'a> {
        let mut offsets = vec![];
        let mut o = 0;
        for e in &c.elems {
            offsets.push(o);
            o += e.n.len();
        }
        Sim {
            c,
            n: c.size(),
            nn: c.nodes.len() - 1,
            hist: vec![Hist::default(); c.elems.len()],
            mem: vec![Mem::default(); c.elems.len()],
            on: vec![true; c.elems.len()],
            gmin: 1e-12,
            scale: 1.0,
            notes: vec![],
            offsets,
        }
    }

    fn volt(x: &[f64], node: usize) -> f64 {
        if node == 0 { 0.0 } else { x[node - 1] }
    }

    fn terms(&self, e: &Elem, x: &[f64]) -> Vec<f64> {
        e.n.iter().map(|&k| Self::volt(x, k)).collect()
    }

    /// The linearised system at `x`; true in the flag when a junction was limited.
    fn assemble(&mut self, x: &[f64], mode: Mode, limit: bool) -> (Vec<f64>, Vec<f64>, bool) {
        let n = self.n;
        let nn = self.nn;
        let mut a = vec![0.0; n * n];
        let mut b = vec![0.0; n];
        let mut limited = false;
        let t = match mode {
            Mode::Dc => 0.0,
            Mode::Tran { t, .. } => t,
        };
        // node k (1..) is row k-1
        let row = |k: usize| if k == 0 { None } else { Some(k - 1) };
        for k in 0..nn {
            a[k * n + k] += self.gmin;
        }
        for (ei, e) in self.c.elems.iter().enumerate() {
            match &e.dev {
                Dev::R(g) => stamp_g(&mut a, n, row(e.n[0]), row(e.n[1]), *g),
                Dev::C(c) => {
                    if let Mode::Tran { dt, hp, .. } = mode {
                        let h = self.hist[ei];
                        let (a0, a1, a2) = gear(dt, hp);
                        let geq = c * a0;
                        let ieq = -c * (a1 * h.v + a2 * h.v1);
                        stamp_g(&mut a, n, row(e.n[0]), row(e.n[1]), geq);
                        if let Some(r) = row(e.n[0]) {
                            b[r] += ieq;
                        }
                        if let Some(r) = row(e.n[1]) {
                            b[r] -= ieq;
                        }
                    }
                }
                Dev::I(w, _) => {
                    let val = w.at(t) * self.scale;
                    if let Some(r) = row(e.n[0]) {
                        b[r] += val;
                    }
                    if let Some(r) = row(e.n[1]) {
                        b[r] -= val;
                    }
                }
                Dev::V(w, _) => {
                    let br = nn + e.br;
                    stamp_branch(&mut a, n, br, row(e.n[0]), row(e.n[1]));
                    b[br] = w.at(t) * self.scale;
                }
                Dev::L(l, r) => {
                    let br = nn + e.br;
                    stamp_branch(&mut a, n, br, row(e.n[0]), row(e.n[1]));
                    let (req, rhs) = match mode {
                        Mode::Dc => (0.0, 0.0),
                        Mode::Tran { dt, hp, .. } => {
                            let h = self.hist[ei];
                            let (a0, a1, a2) = gear(dt, hp);
                            (l * a0, l * (a1 * h.i + a2 * h.i1))
                        }
                    };
                    a[br * n + br] -= r + req;
                    b[br] = rhs;
                }
                dev => {
                    let v = self.terms(e, x);
                    let lin = eval(dev, &v, &mut self.mem[ei], self.on[ei], limit);
                    limited |= lin.limited;
                    let k = e.n.len();
                    for r in 0..k {
                        let Some(rr) = row(e.n[r]) else { continue };
                        let mut ju = 0.0;
                        for c in 0..k {
                            let g = lin.j[r * k + c];
                            ju += g * lin.u[c];
                            if let Some(cc) = row(e.n[c]) {
                                a[rr * n + cc] += g;
                            }
                        }
                        b[rr] -= lin.i[r] - ju;
                    }
                }
            }
        }
        (a, b, limited)
    }

    fn newton(&mut self, x: &mut [f64], mode: Mode, maxit: usize) -> bool {
        let n = self.n;
        for it in 0..maxit {
            let (mut a, mut b, limited) = self.assemble(x, mode, true);
            if !lu_solve(n, &mut a, &mut b) {
                return false;
            }
            if b.iter().any(|v| !v.is_finite()) {
                return false;
            }
            let mut ok = !limited;
            for k in 0..n {
                let abstol = if k < self.nn { 1e-6 } else { 1e-9 };
                if (b[k] - x[k]).abs() > abstol + 1e-4 * b[k].abs().max(x[k].abs()) {
                    ok = false;
                }
            }
            x.copy_from_slice(&b);
            if ok && it > 0 {
                return true;
            }
        }
        false
    }

    /// A DC solution from `x`, falling back to gmin and then source stepping.
    fn dc(&mut self, x: &mut Vec<f64>) -> Result<(), String> {
        let start = x.clone();
        if self.newton(x, Mode::Dc, 150) {
            return Ok(());
        }
        // gmin stepping
        x.copy_from_slice(&start);
        let mut ok = true;
        for e in 3..=12 {
            self.gmin = 10f64.powi(-e);
            if !self.newton(x, Mode::Dc, 150) {
                ok = false;
                break;
            }
        }
        self.gmin = 1e-12;
        if ok && self.newton(x, Mode::Dc, 150) {
            return Ok(());
        }
        // source stepping
        x.iter_mut().for_each(|v| *v = 0.0);
        let mut s = 0.0f64;
        let mut ds = 0.1f64;
        while s < 1.0 {
            let next = (s + ds).min(1.0);
            self.scale = next;
            let keep = x.clone();
            if self.newton(x, Mode::Dc, 150) {
                s = next;
                ds = (ds * 1.5).min(0.25);
            } else {
                x.copy_from_slice(&keep);
                ds /= 4.0;
                if ds < 1e-4 {
                    self.scale = 1.0;
                    return Err("the DC operating point did not converge".into());
                }
            }
        }
        self.scale = 1.0;
        Ok(())
    }

    /// Update every 555's flip-flop from `x`; true when one changed.
    fn update_timers(&mut self, x: &[f64]) -> bool {
        let mut changed = false;
        for (ei, e) in self.c.elems.iter().enumerate() {
            if let Dev::T555(_) = e.dev {
                let v = self.terms(e, x);
                let s = timer_next(&v, self.on[ei]);
                changed |= s != self.on[ei];
                self.on[ei] = s;
            }
        }
        changed
    }

    fn timers_next(&self, x: &[f64]) -> Vec<bool> {
        self.c.elems.iter().enumerate().map(|(ei, e)| if let Dev::T555(_) = e.dev { timer_next(&self.terms(e, x), self.on[ei]) } else { self.on[ei] }).collect()
    }

    /// The DC operating point (capacitors open, inductors shorted).
    pub fn op(&mut self) -> Result<Vec<f64>, String> {
        let mut x = vec![0.0; self.n];
        self.on.iter_mut().for_each(|s| *s = true);
        for pass in 0..8 {
            self.dc(&mut x)?;
            let before = self.on.clone();
            if !self.update_timers(&x) {
                return Ok(x);
            }
            if pass == 7 {
                // keep the state that x was solved with
                self.on = before;
            }
        }
        self.notes.push("a 555 has no steady DC state (it oscillates); the operating point shows one of its states".into());
        Ok(x)
    }

    /// Every element's terminal currents at `x` (after `accept` in a transient).
    pub fn currents(&mut self, x: &[f64], dc: bool) -> Vec<f64> {
        let mut out = vec![];
        let nn = self.nn;
        for (ei, e) in self.c.elems.iter().enumerate() {
            let v = self.terms(e, x);
            match &e.dev {
                Dev::R(g) => {
                    let i = g * (v[0] - v[1]);
                    out.extend([i, -i]);
                }
                Dev::C(_) => {
                    let i = if dc { 0.0 } else { self.hist[ei].i };
                    out.extend([i, -i]);
                }
                Dev::I(..) => {
                    let i = self.cur_i(ei);
                    out.extend([-i, i]);
                }
                Dev::V(..) | Dev::L(..) => {
                    let j = x[nn + e.br];
                    out.extend([j, -j]);
                }
                dev => {
                    let mut m = self.mem[ei];
                    let lin = eval(dev, &v, &mut m, self.on[ei], false);
                    out.extend(lin.i);
                }
            }
        }
        out
    }

    /// A current source's value at the last accepted time (kept in its history by `accept`).
    fn cur_i(&self, ei: usize) -> f64 {
        match &self.c.elems[ei].dev {
            Dev::I(w, _) => w.at(self.hist[ei].v) * self.scale,
            _ => 0.0,
        }
    }

    fn accept(&mut self, x: &[f64], mode: Mode) {
        for (ei, e) in self.c.elems.iter().enumerate() {
            let v = self.terms(e, x);
            match &e.dev {
                Dev::C(c) => {
                    let h = self.hist[ei];
                    let vn = v[0] - v[1];
                    let i = match mode {
                        Mode::Tran { dt, hp, .. } => {
                            let (a0, a1, a2) = gear(dt, hp);
                            c * (a0 * vn + a1 * h.v + a2 * h.v1)
                        }
                        Mode::Dc => 0.0,
                    };
                    self.hist[ei] = Hist { v: vn, i, v1: h.v, i1: 0.0 };
                }
                Dev::L(_, r) => {
                    let h = self.hist[ei];
                    let j = x[self.nn + e.br];
                    self.hist[ei] = Hist { v: v[0] - v[1] - r * j, i: j, v1: 0.0, i1: h.i };
                }
                Dev::I(..) => {
                    // remember the time for `currents`
                    if let Mode::Tran { t, .. } = mode {
                        self.hist[ei].v = t;
                    }
                }
                _ => {}
            }
        }
    }

    fn vscale(&self) -> f64 {
        self.c.elems.iter().filter_map(|e| if let Dev::V(w, _) = &e.dev { Some(w.peak()) } else { None }).fold(1.0, f64::max)
    }

    /// A transient from 0 to `stop` with steps no longer than `hmax`. From rest (every capacitor
    /// discharged, supplies switched on at 0) unless `from_op`.
    pub fn tran(&mut self, stop: f64, hmax: f64, from_op: bool) -> Result<Tran, String> {
        if !(stop > 0.0) {
            return Err("the stop time must be positive".into());
        }
        let mut hmax = if hmax > 0.0 { hmax.min(stop / 10.0) } else { stop / 500.0 };
        for e in &self.c.elems {
            if let Dev::V(w, _) | Dev::I(w, _) = &e.dev {
                hmax = hmax.min(w.max_step());
            }
        }
        let hmin = (stop * 1e-13).max(1e-18);
        let ev_tol = (stop * 1e-8).max(1e-12);
        let vscale = self.vscale();
        let mut x = if from_op { self.op()? } else { vec![0.0; self.n] };
        if !from_op {
            self.on.iter_mut().for_each(|s| *s = true);
        }
        // start the history from x
        for (ei, e) in self.c.elems.iter().enumerate() {
            let v = self.terms(e, &x);
            self.hist[ei] = match &e.dev {
                Dev::C(_) => Hist { v: v[0] - v[1], ..Default::default() },
                Dev::L(..) => Hist { i: x[self.nn + e.br], ..Default::default() },
                _ => Hist::default(),
            };
        }
        let mut out = Tran { t: vec![0.0], x: vec![x.clone()], i: vec![] };
        let i0 = self.currents(&x, from_op);
        out.i.push(if from_op { i0 } else { vec![0.0; i0.len()] });
        let mut t = 0.0;
        let mut dt = (hmax * 1e-3).max(hmin * 10.0);
        let mut be = true;
        let mut hp = 0.0;
        let mut steps = 0usize;
        while t < stop * (1.0 - 1e-12) {
            steps += 1;
            if steps > 2_000_000 {
                return Err(format!("the transient took too many steps (stopped at t = {})", crate::circuit::si(t, "s")));
            }
            let bp = self.c.elems.iter().filter_map(|e| if let Dev::V(w, _) | Dev::I(w, _) = &e.dev { Some(w.next_break(t)) } else { None }).fold(f64::INFINITY, f64::min);
            let mut h = dt.min(hmax).min(stop - t);
            let mut at_bp = false;
            if bp - t <= h * (1.0 + 1e-9) {
                h = bp - t;
                at_bp = true;
            }
            let mode = Mode::Tran { t: t + h, dt: h, hp: if be { 0.0 } else { hp } };
            let mut xn = x.clone();
            let mem = self.mem.clone();
            if !self.newton(&mut xn, mode, 60) {
                self.mem = mem;
                dt = h / 8.0;
                if dt < hmin {
                    return Err(format!("the transient failed to converge at t = {}", crate::circuit::si(t, "s")));
                }
                continue;
            }
            let dv = (0..self.nn).map(|k| (xn[k] - x[k]).abs()).fold(0.0, f64::max);
            if !be && dv > 0.5 * vscale && h > hmin * 1e3 {
                self.mem = mem;
                dt = h / 4.0;
                continue;
            }
            let next = self.timers_next(&xn);
            let switched = next != self.on;
            if switched && h > ev_tol {
                self.mem = mem;
                dt = h / 2.0;
                continue;
            }
            t += h;
            self.accept(&xn, mode);
            let i = self.currents(&xn, false);
            out.t.push(t);
            out.x.push(xn.clone());
            out.i.push(i);
            x = xn;
            hp = h;
            if switched {
                self.on = next;
                be = true;
                dt = ev_tol * 10.0;
            } else if at_bp {
                be = true;
                dt = (hmax / 100.0).max(hmin * 10.0);
            } else {
                be = false;
                let grow = if dv < 0.02 * vscale {
                    2.0
                } else if dv < 0.08 * vscale {
                    1.25
                } else {
                    0.6
                };
                dt = (h * grow).min(hmax);
            }
        }
        Ok(out)
    }

    /// Small-signal response about `op` at each frequency: the unknowns and the terminal currents.
    pub fn ac(&mut self, op: &[f64], freqs: &[f64]) -> Result<(Vec<Vec<Cx>>, Vec<Vec<Cx>>), String> {
        let n = self.n;
        let nn = self.nn;
        let (a, _, _) = self.assemble(op, Mode::Dc, false);
        let row = |k: usize| if k == 0 { None } else { Some(k - 1) };
        let mut rhs = vec![Cx::default(); n];
        let mut any = false;
        for e in &self.c.elems {
            match &e.dev {
                Dev::V(_, ac) if *ac != 0.0 => {
                    rhs[nn + e.br] = Cx::new(*ac, 0.0);
                    any = true;
                }
                Dev::I(_, ac) if *ac != 0.0 => {
                    if let Some(r) = row(e.n[0]) {
                        rhs[r] = rhs[r] + Cx::new(*ac, 0.0);
                    }
                    if let Some(r) = row(e.n[1]) {
                        rhs[r] = rhs[r] - Cx::new(*ac, 0.0);
                    }
                    any = true;
                }
                _ => {}
            }
        }
        if !any {
            return Err("no source has an AC magnitude: give the input source ac 1".into());
        }
        // each nonlinear device's Jacobian at the operating point, for its currents
        let lins: Vec<Option<Lin>> = self
            .c
            .elems
            .iter()
            .enumerate()
            .map(|(ei, e)| match &e.dev {
                Dev::R(_) | Dev::C(_) | Dev::I(..) | Dev::V(..) | Dev::L(..) => None,
                dev => {
                    let mut m = self.mem[ei];
                    Some(eval(dev, &self.terms(e, op), &mut m, self.on[ei], false))
                }
            })
            .collect();
        let mut xs = vec![];
        let mut is = vec![];
        for &f in freqs {
            let w = 2.0 * std::f64::consts::PI * f;
            let mut m: Vec<Cx> = a.iter().map(|v| Cx::new(*v, 0.0)).collect();
            for e in &self.c.elems {
                match &e.dev {
                    Dev::C(c) => {
                        let y = w * c;
                        let (p, q) = (row(e.n[0]), row(e.n[1]));
                        for (r, s) in [(p, 1.0), (q, -1.0)] {
                            let Some(r) = r else { continue };
                            for (cc, t) in [(p, 1.0), (q, -1.0)] {
                                if let Some(cc) = cc {
                                    m[r * n + cc].im += s * t * y;
                                }
                            }
                        }
                    }
                    Dev::L(l, _) => {
                        let br = nn + e.br;
                        m[br * n + br].im -= w * l;
                    }
                    _ => {}
                }
            }
            let mut b = rhs.clone();
            if !lu_solve(n, &mut m, &mut b) {
                return Err("the AC system is singular".into());
            }
            let v = |k: usize| if k == 0 { Cx::default() } else { b[k - 1] };
            let mut cur = vec![];
            for (ei, e) in self.c.elems.iter().enumerate() {
                match &e.dev {
                    Dev::R(g) => {
                        let i = (v(e.n[0]) - v(e.n[1])) * Cx::new(*g, 0.0);
                        cur.extend([i, -i]);
                    }
                    Dev::C(c) => {
                        let i = (v(e.n[0]) - v(e.n[1])) * Cx::new(0.0, w * c);
                        cur.extend([i, -i]);
                    }
                    Dev::I(_, ac) => {
                        let i = Cx::new(*ac, 0.0);
                        cur.extend([-i, i]);
                    }
                    Dev::V(..) | Dev::L(..) => {
                        let j = b[nn + e.br];
                        cur.extend([j, -j]);
                    }
                    _ => {
                        let lin = lins[ei].as_ref().unwrap();
                        let k = e.n.len();
                        for r in 0..k {
                            let mut s = Cx::default();
                            for c in 0..k {
                                s = s + v(e.n[c]) * Cx::new(lin.j[r * k + c], 0.0);
                            }
                            cur.push(s);
                        }
                    }
                }
            }
            xs.push(b);
            is.push(cur);
        }
        Ok((xs, is))
    }
}

fn stamp_g(a: &mut [f64], n: usize, p: Option<usize>, q: Option<usize>, g: f64) {
    if let Some(p) = p {
        a[p * n + p] += g;
    }
    if let Some(q) = q {
        a[q * n + q] += g;
    }
    if let (Some(p), Some(q)) = (p, q) {
        a[p * n + q] -= g;
        a[q * n + p] -= g;
    }
}

/// A branch current unknown `br` flowing from p through the element to q, with the row
/// v(p) - v(q) (the caller adds the rest of the row).
fn stamp_branch(a: &mut [f64], n: usize, br: usize, p: Option<usize>, q: Option<usize>) {
    if let Some(p) = p {
        a[p * n + br] += 1.0;
        a[br * n + p] += 1.0;
    }
    if let Some(q) = q {
        a[q * n + br] -= 1.0;
        a[br * n + q] -= 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn divider() -> Circuit {
        let mut c = Circuit::new();
        let a = c.node("a");
        let b = c.node("b");
        c.add(Dev::V(Wave::Dc(10.0), 1.0), &[a, 0]);
        c.add(Dev::R(1.0 / 1000.0), &[a, b]);
        c.add(Dev::R(1.0 / 3000.0), &[b, 0]);
        c
    }

    #[test]
    fn divider_op() {
        let c = divider();
        let x = Sim::new(&c).op().unwrap();
        assert!((x[1] - 7.5).abs() < 1e-6, "{x:?}");
        // the source delivers 2.5 mA (its branch current flows the other way)
        assert!((x[2] + 2.5e-3).abs() < 1e-9);
    }

    #[test]
    fn diode_drop() {
        let mut c = Circuit::new();
        let a = c.node("a");
        let k = c.node("k");
        c.add(Dev::V(Wave::Dc(5.0), 0.0), &[a, 0]);
        c.add(Dev::R(1.0 / 1000.0), &[a, k]);
        c.add(Dev::D(Diode { is: 2.52e-9, n: 1.752, bv: f64::INFINITY, ibv: 0.0 }), &[k, 0]);
        let x = Sim::new(&c).op().unwrap();
        assert!(x[1] > 0.6 && x[1] < 0.75, "{}", x[1]);
    }

    #[test]
    fn rc_charges() {
        let mut c = Circuit::new();
        let a = c.node("a");
        let b = c.node("b");
        c.add(Dev::V(Wave::Dc(1.0), 0.0), &[a, 0]);
        c.add(Dev::R(1.0 / 1000.0), &[a, b]);
        c.add(Dev::C(1e-6), &[b, 0]);
        let tr = Sim::new(&c).tran(5e-3, 0.0, false).unwrap();
        // at one time constant 63 %
        let k = tr.t.iter().position(|t| *t >= 1e-3).unwrap();
        let v = tr.x[k][1];
        assert!((v - 0.632).abs() < 0.02, "{v} at {}", tr.t[k]);
        assert!((tr.x.last().unwrap()[1] - 0.993).abs() < 0.01);
    }

    #[test]
    fn rc_lowpass_ac() {
        let mut c = Circuit::new();
        let a = c.node("a");
        let b = c.node("b");
        c.add(Dev::V(Wave::Dc(0.0), 1.0), &[a, 0]);
        c.add(Dev::R(1.0 / 1000.0), &[a, b]);
        c.add(Dev::C(100e-9), &[b, 0]);
        let mut s = Sim::new(&c);
        let op = s.op().unwrap();
        let fc = 1.0 / (2.0 * std::f64::consts::PI * 1000.0 * 100e-9);
        let (xs, _) = s.ac(&op, &[fc]).unwrap();
        let g = xs[0][1].abs();
        assert!((g - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3, "{g}");
        assert!((xs[0][1].arg().to_degrees() + 45.0).abs() < 0.1);
    }

    #[test]
    fn lc_rings_at_resonance() {
        let mut c = Circuit::new();
        let a = c.node("a");
        let b = c.node("b");
        // a step into a series LC: the capacitor rings at 1/(2π√LC)
        c.add(Dev::V(Wave::Pulse { v1: 0.0, v2: 1.0, td: 0.0, tr: 1e-9, tf: 1e-9, pw: 1.0, per: 0.0 }, 0.0), &[a, 0]);
        c.add(Dev::L(1e-3, 1.0), &[a, b]);
        c.add(Dev::C(1e-6), &[b, 0]);
        let tr = Sim::new(&c).tran(2e-3, 1e-6, false).unwrap();
        let f0 = 1.0 / (2.0 * std::f64::consts::PI * (1e-3f64 * 1e-6).sqrt());
        // first peak at half a period, near 2 V
        let (k, vmax) = tr.x.iter().enumerate().map(|(k, x)| (k, x[1])).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
        assert!((tr.t[k] - 0.5 / f0).abs() < 0.03 / f0, "{} vs {}", tr.t[k], 0.5 / f0);
        assert!(vmax > 1.8 && vmax < 2.0, "{vmax}");
    }

    #[test]
    fn opamp_inverting_gain() {
        let mut c = Circuit::new();
        let vin = c.node("in");
        let inv = c.node("inv");
        let out = c.node("out");
        let vp = c.node("vp");
        let vn = c.node("vn");
        let int = c.node("int");
        c.add(Dev::V(Wave::Dc(0.1), 1.0), &[vin, 0]);
        c.add(Dev::V(Wave::Dc(12.0), 0.0), &[vp, 0]);
        c.add(Dev::V(Wave::Dc(-12.0), 0.0), &[vn, 0]);
        c.add(Dev::R(1.0 / 1e3), &[vin, inv]);
        c.add(Dev::R(1.0 / 10e3), &[inv, out]);
        let m = OpAmp { a0: 1e5, gbw: 1e6, sr: 0.5e6, hr_hi: 1.5, hr_lo: 1.5, ro: 50.0 };
        let ci = m.cint();
        c.add(Dev::Op(m), &[0, inv, out, vp, vn, int]);
        c.add(Dev::C(ci), &[int, 0]);
        let mut s = Sim::new(&c);
        let x = s.op().unwrap();
        assert!((x[out - 1] + 1.0).abs() < 2e-3, "{}", x[out - 1]);
        // the closed-loop bandwidth is GBW / (1 + 10)
        let (xs, _) = s.ac(&x, &[100.0, 1e6 / 11.0]).unwrap();
        assert!((xs[0][out - 1].abs() - 10.0).abs() < 0.05);
        let g = xs[1][out - 1].abs();
        assert!(g > 6.0 && g < 8.0, "{g}");
        // saturates at the rail less the headroom
        let mut c2 = c.clone();
        c2.elems[0].dev = Dev::V(Wave::Dc(-5.0), 0.0);
        let x = Sim::new(&c2).op().unwrap();
        assert!((x[out - 1] - 10.5).abs() < 0.2, "{}", x[out - 1]);
    }

    #[test]
    fn bjt_switch_saturates() {
        let mut c = Circuit::new();
        let vcc = c.node("vcc");
        let b = c.node("b");
        let col = c.node("c");
        c.add(Dev::V(Wave::Dc(5.0), 0.0), &[vcc, 0]);
        c.add(Dev::R(1.0 / 10e3), &[vcc, b]);
        c.add(Dev::R(1.0 / 1e3), &[vcc, col]);
        c.add(Dev::Q(Bjt { is: 6.7e-15, bf: 416.0, br: 0.74, vaf: 74.0, nf: 1.0 }, false), &[col, b, 0]);
        let x = Sim::new(&c).op().unwrap();
        assert!(x[b - 1] > 0.6 && x[b - 1] < 0.8, "vbe {}", x[b - 1]);
        assert!(x[col - 1] < 0.3, "vce {}", x[col - 1]);
    }

    #[test]
    fn nmos_follows_square_law() {
        let mut c = Circuit::new();
        let vdd = c.node("vdd");
        let g = c.node("g");
        let d = c.node("d");
        c.add(Dev::V(Wave::Dc(10.0), 0.0), &[vdd, 0]);
        c.add(Dev::V(Wave::Dc(3.0), 0.0), &[g, 0]);
        c.add(Dev::R(1.0 / 100.0), &[vdd, d]);
        c.add(Dev::M(Mos { vto: 2.0, kp: 0.1, lambda: 0.0 }, false), &[d, g, 0]);
        let x = Sim::new(&c).op().unwrap();
        // saturation: 0.05 A → 5 V across the resistor
        assert!((x[d - 1] - 5.0).abs() < 0.05, "{}", x[d - 1]);
    }
}
