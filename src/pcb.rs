//! PCB tabs: a schematic, the netlist its wires make and the two-layer board it is laid out on.
//!
//! Parts are shared by both views: a part has a schematic symbol and a footprint, and its pins
//! are numbered like the footprint's pads. The netlist is worked out from the schematic each time
//! (wires, junctions and same-named labels), so the board always shows the current connections.
//! Units are millimetres with y up.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::cam_paths::{Contour, Shapes};
use crate::pcb_lib::{self, Footprint, PadShape, Symbol, P};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Pcb {
    pub name: String,
    #[serde(default)]
    pub parts: Vec<Part>,
    /// Schematic wires.
    #[serde(default)]
    pub wires: Vec<Wire>,
    /// Net labels and power ports on the schematic.
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub board: Board,
    /// No-connect marks on the schematic (pins left open on purpose).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_connects: Vec<P>,
    /// Pin electrical types set by hand ("U1.3": "output"), over the ones the library gives.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pin_types: std::collections::BTreeMap<String, crate::pcb_erc::PinType>,
    /// Circuit simulation: sources, ground, analysis.
    #[serde(default, skip_serializing_if = "crate::circuit::SimSetup::is_empty")]
    pub sim: crate::circuit::SimSetup,
    /// Output job: which fabrication and documentation outputs a run writes.
    #[serde(default, skip_serializing_if = "crate::pcb_fab::OutJob::is_default")]
    pub outputs: crate::pcb_fab::OutJob,
    #[serde(default)]
    pub next_id: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub id: u64,
    pub reference: String,
    pub value: String,
    /// Library symbol key (`R`, `C`, `U`, …).
    pub symbol: String,
    /// Footprint name (`R_0805`, `SOIC-8`, …).
    pub footprint: String,
    /// Pin names for ICs and connectors, in pin order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin_names: Vec<String>,
    /// Manufacturer part number, for the BOM.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mpn: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manufacturer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Datasheet link.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub datasheet: String,
    /// Digi-Key part number (`311-10.0KCRCT-ND`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digikey: String,
    /// The supplier's package name (`0805 (2012 Metric)`), which picked the footprint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub package: String,
    /// More fields for the BOM, in order (tolerance, voltage, power, LCSC number …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, String)>,
    /// Simulation model overrides, by parameter key (`bf` → `300`); see `circuit::knobs`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sim: std::collections::BTreeMap<String, String>,
    /// Left out of the circuit simulation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_sim: bool,
    /// Schematic position, quarter turns anticlockwise and mirror (about the vertical axis).
    pub sch: P,
    #[serde(default)]
    pub sch_rot: u8,
    #[serde(default)]
    pub sch_mirror: bool,
    /// Board position (footprint origin), rotation in degrees and side.
    pub at: P,
    #[serde(default)]
    pub rot: f64,
    #[serde(default)]
    pub bottom: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Wire {
    pub a: P,
    pub b: P,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LabelKind {
    #[default]
    Net,
    /// A supply port (a bar above the point).
    Power,
    /// A ground port (the ground symbol below the point).
    Ground,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Label {
    pub at: P,
    pub text: String,
    #[serde(default)]
    pub kind: LabelKind,
    /// Quarter turns (net labels only).
    #[serde(default)]
    pub rot: u8,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    #[default]
    Top,
    Bottom,
}

impl Layer {
    pub fn other(self) -> Layer {
        match self {
            Layer::Top => Layer::Bottom,
            Layer::Bottom => Layer::Top,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Layer::Top => "Top",
            Layer::Bottom => "Bottom",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub a: P,
    pub b: P,
    pub width: f64,
    pub layer: Layer,
    #[serde(default)]
    pub net: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Via {
    pub at: P,
    pub d: f64,
    pub drill: f64,
    #[serde(default)]
    pub net: String,
}

/// How a pour joins the pads of its own net.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PadConnect {
    /// Thermal reliefs on through-hole pads (so they solder), solid on surface mount pads.
    #[default]
    ThtRelief,
    /// Thermal reliefs on every pad.
    Relief,
    /// Solid copper to every pad.
    Solid,
}

/// A copper pour: the outline is filled with copper of its net, kept clear of other nets.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Zone {
    pub net: String,
    pub layer: Layer,
    pub outline: Vec<P>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pads: PadConnect,
    /// Gap to other nets' copper; 0 takes the net's clearance.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub clearance: f64,
    /// Thermal relief: the gap round the pad and the spoke width; 0 takes the defaults (0.3 mm,
    /// and the net's track width).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub relief_gap: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spoke: f64,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

fn yes() -> bool {
    true
}

/// An area things are kept out of (Altium's keepouts and room rules).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Keepout {
    pub outline: Vec<P>,
    /// The copper layer it applies to; None for both.
    #[serde(default)]
    pub layer: Option<Layer>,
    #[serde(default = "yes")]
    pub tracks: bool,
    #[serde(default = "yes")]
    pub vias: bool,
    #[serde(default = "yes")]
    pub pours: bool,
    /// Parts may not be placed in it.
    #[serde(default)]
    pub parts: bool,
}

impl Default for Keepout {
    fn default() -> Self {
        Keepout { outline: vec![], layer: None, tracks: true, vias: true, pours: true, parts: false }
    }
}

impl Keepout {
    pub fn on(&self, l: Option<Layer>) -> bool {
        match (self.layer, l) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }
}

/// Nets that share routing rules (Altium's net classes): their own clearance, track width and via.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NetClass {
    pub name: String,
    /// Net names in the class; a trailing `*` matches every net that starts with the rest.
    pub nets: Vec<String>,
    /// 0 for any of these takes the board's rule.
    pub clearance: f64,
    pub track: f64,
    pub via_d: f64,
    pub via_drill: f64,
    /// Targets for the trace calculator: the current the nets carry (A) and their impedance (Ω);
    /// 0 for none.
    pub current: f64,
    pub impedance: f64,
}

/// Two nets routed side by side at a fixed gap (USB D+/D-, LVDS, CAN).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DiffPair {
    pub name: String,
    /// The positive and negative nets.
    pub p: String,
    pub n: String,
    /// Track width and the gap between the two tracks; 0 takes the net's track width and its
    /// clearance.
    pub width: f64,
    pub gap: f64,
}

/// A length rule: the nets' routed length must lie in [min, max] (0 = no bound), and when
/// `tolerance` is set they must all be within it of the longest (a matched-length group).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LengthRule {
    pub name: String,
    pub nets: Vec<String>,
    pub min: f64,
    pub max: f64,
    pub tolerance: f64,
}

/// The rules a net is routed and checked with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetRules {
    pub clearance: f64,
    pub track: f64,
    pub via_d: f64,
    pub via_drill: f64,
}

/// Whether a net name matches a pattern (`M1_*` is a prefix match), ignoring case.
pub fn net_matches(pat: &str, net: &str) -> bool {
    let pat = pat.trim();
    match pat.strip_suffix('*') {
        Some(pre) => net.to_ascii_uppercase().starts_with(&pre.to_ascii_uppercase()),
        None => !pat.is_empty() && net.eq_ignore_ascii_case(pat),
    }
}

/// An unplated mounting hole.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Hole {
    pub at: P,
    pub d: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Rules {
    /// Copper to copper of another net.
    pub clearance: f64,
    /// Width of new tracks, and of power tracks (nets that labels mark as power or ground).
    pub track: f64,
    pub power_track: f64,
    pub via_d: f64,
    pub via_drill: f64,
    /// Copper to the board edge.
    pub edge: f64,
    pub min_track: f64,
    pub min_drill: f64,
    pub min_annular: f64,
    /// More nets that get the power track width, by name (`M1_*` ends in a prefix match): motor
    /// phases, heater and LED strip outputs.
    pub power_nets: Vec<String>,
    /// Net classes, the first that names a net wins.
    pub classes: Vec<NetClass>,
    /// Hole edge to hole edge (pads, vias, mounting holes).
    pub hole_gap: f64,
    /// Silkscreen to exposed pad copper.
    pub silk_gap: f64,
    /// Tallest part allowed above (and below) the board, 0 for no limit.
    pub max_height: f64,
    /// Gap kept between parts' 3D bodies.
    pub body_gap: f64,
    /// Checks turned off, by id (see `CHECKS`).
    pub off: Vec<String>,
    /// Differential pairs.
    pub pairs: Vec<DiffPair>,
    /// Length and matched-length rules.
    pub lengths: Vec<LengthRule>,
}

/// The design rule and electrical rule checks, by id, with what each looks for.
pub const CHECKS: &[(&str, &str)] = &[
    ("clearance", "Copper closer to another net than its clearance"),
    ("short", "Copper of two nets touching"),
    ("width", "Tracks under the minimum width"),
    ("class_width", "Tracks narrower than their net class"),
    ("via", "Via drill and annular ring"),
    ("hole_gap", "Holes too close to each other"),
    ("edge", "Copper near or off the board edge"),
    ("keepout", "Copper, vias or parts in a keepout"),
    ("silk", "Silkscreen over exposed pads"),
    ("overlap", "Courtyards that overlap"),
    ("body", "3D bodies that collide"),
    ("height", "Parts taller than the height limit"),
    ("outside", "Parts not inside the board"),
    ("antenna", "Track ends that connect to nothing"),
    ("thermal", "Thermal reliefs with fewer than two spokes"),
    ("unrouted", "Connections not routed yet"),
    ("length", "Nets outside their length rule"),
    ("matched", "Matched-length nets out of tolerance"),
    ("pair", "Differential pairs with uneven lengths or the wrong gap"),
    ("erc_unconnected", "Pins left unconnected (no no-connect mark)"),
    ("erc_single", "Nets with one pin"),
    ("erc_drivers", "Outputs driving each other"),
    ("erc_power", "Power pins with no supply"),
    ("erc_undriven", "Inputs nothing drives"),
    ("erc_nc", "No-connect marks on connected pins"),
    ("erc_labels", "Labels and wire ends that join nothing"),
    ("erc_refs", "Duplicate references"),
    ("erc_footprint", "Unknown footprints"),
];

impl Rules {
    pub fn is_power_net(&self, net: &str) -> bool {
        self.power_nets.iter().any(|p| net_matches(p, net))
    }
    /// The class a net is in.
    pub fn class_of(&self, net: &str) -> Option<&NetClass> {
        if net.is_empty() {
            return None;
        }
        self.classes.iter().find(|c| c.nets.iter().any(|p| net_matches(p, net)))
    }
    /// The rules for a net; `power` when the schematic names it with a power or ground port.
    pub fn for_net(&self, net: &str, power: bool) -> NetRules {
        let power = power || self.is_power_net(net);
        let base = NetRules { clearance: self.clearance, track: if power { self.power_track } else { self.track }, via_d: self.via_d, via_drill: self.via_drill };
        let Some(c) = self.class_of(net) else { return base };
        let or = |v: f64, d: f64| if v > 0.0 { v } else { d };
        NetRules { clearance: or(c.clearance, base.clearance), track: or(c.track, base.track), via_d: or(c.via_d, base.via_d), via_drill: or(c.via_drill, base.via_drill) }
    }
    pub fn checks(&self, id: &str) -> bool {
        !self.off.iter().any(|o| o == id)
    }
}

impl Default for Rules {
    fn default() -> Self {
        // what common low-cost fabs make without extra charge
        Rules {
            clearance: 0.2,
            track: 0.25,
            power_track: 0.5,
            via_d: 0.6,
            via_drill: 0.3,
            edge: 0.3,
            min_track: 0.15,
            min_drill: 0.3,
            min_annular: 0.13,
            power_nets: vec![],
            classes: vec![],
            hole_gap: 0.25,
            silk_gap: 0.0,
            max_height: 0.0,
            body_gap: 0.0,
            off: vec![],
            pairs: vec![],
            lengths: vec![],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Board {
    /// Closed outline, anticlockwise.
    pub outline: Vec<P>,
    #[serde(default = "thickness")]
    pub thickness: f64,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub vias: Vec<Via>,
    #[serde(default)]
    pub zones: Vec<Zone>,
    #[serde(default)]
    pub holes: Vec<Hole>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keepouts: Vec<Keepout>,
    #[serde(default)]
    pub rules: Rules,
    /// Solder mask colour name (green, black, blue, red, white, purple), for previews.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
}

fn thickness() -> f64 {
    1.6
}

impl Default for Board {
    fn default() -> Self {
        Board { outline: rect_outline(50.0, 40.0, 0.0), thickness: 1.6, tracks: vec![], vias: vec![], zones: vec![], holes: vec![], keepouts: vec![], rules: Rules::default(), color: String::new() }
    }
}

/// A w × h board outline from the origin, with corners rounded to `r`.
pub fn rect_outline(w: f64, h: f64, r: f64) -> Vec<P> {
    let r = r.clamp(0.0, w.min(h) / 2.0);
    if r < 1e-6 {
        return vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
    }
    let mut v = vec![];
    for (c, a0) in [([w - r, r], -90.0), ([w - r, h - r], 0.0), ([r, h - r], 90.0), ([r, r], 180.0)] {
        for i in 0..=6 {
            let a = (a0 + 15.0 * i as f64).to_radians();
            v.push([c[0] + r * a.cos(), c[1] + r * a.sin()]);
        }
    }
    v
}

impl Board {
    pub fn bbox(&self) -> (P, P) {
        bbox(&self.outline)
    }
    pub fn size(&self) -> P {
        let (lo, hi) = self.bbox();
        [hi[0] - lo[0], hi[1] - lo[1]]
    }
    pub fn mask_rgb(&self) -> [u8; 3] {
        match self.color.as_str() {
            "black" => [30, 30, 34],
            "blue" => [20, 60, 140],
            "red" => [170, 30, 30],
            "white" => [235, 235, 235],
            "purple" => [90, 40, 130],
            _ => [20, 110, 50],
        }
    }
}

pub fn bbox(pts: &[P]) -> (P, P) {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in pts {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    if !lo[0].is_finite() {
        return ([0.0; 2], [0.0; 2]);
    }
    (lo, hi)
}

// ---------------------------------------------------------------- placement transforms

fn rot_q(p: P, q: u8) -> P {
    match q % 4 {
        0 => p,
        1 => [-p[1], p[0]],
        2 => [-p[0], -p[1]],
        _ => [p[1], -p[0]],
    }
}

fn rot_deg(p: P, deg: f64) -> P {
    let (s, c) = deg.to_radians().sin_cos();
    [p[0] * c - p[1] * s, p[0] * s + p[1] * c]
}

pub fn add(a: P, b: P) -> P {
    [a[0] + b[0], a[1] + b[1]]
}
pub fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
pub fn dist(a: P, b: P) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

impl Part {
    /// Schematic: symbol coordinates to sheet.
    pub fn sch_xf(&self, p: P) -> P {
        let p = if self.sch_mirror { [-p[0], p[1]] } else { p };
        add(rot_q(p, self.sch_rot), self.sch)
    }
    /// Schematic: a symbol direction to sheet.
    pub fn sch_dir(&self, d: P) -> P {
        let d = if self.sch_mirror { [-d[0], d[1]] } else { d };
        rot_q(d, self.sch_rot)
    }
    /// Board: footprint coordinates to board (bottom-side parts are mirrored, seen from the top).
    pub fn xf(&self, p: P) -> P {
        let p = if self.bottom { [-p[0], p[1]] } else { p };
        add(rot_deg(p, self.rot), self.at)
    }
    pub fn footprint(&self) -> Footprint {
        pcb_lib::footprint(&self.footprint).unwrap_or_else(|| pcb_lib::footprint("R_0805").unwrap())
    }
    pub fn symbol(&self) -> Symbol {
        let fp = self.footprint();
        pcb_lib::symbol(&self.symbol, &fp.pin_numbers(), &self.pin_names)
    }
    pub fn side(&self) -> Layer {
        if self.bottom { Layer::Bottom } else { Layer::Top }
    }
}

/// A pad placed on the board.
#[derive(Clone, Debug)]
pub struct WPad {
    pub part: usize,
    pub num: String,
    pub at: P,
    pub size: P,
    pub shape: PadShape,
    pub rot: f64,
    pub drill: f64,
    /// Surface mount pads are on their part's side; plated holes on both.
    pub layer: Option<Layer>,
    pub net: Option<usize>,
}

impl WPad {
    pub fn on(&self, l: Layer) -> bool {
        self.layer.is_none_or(|x| x == l)
    }
    /// The pad's copper as a shape.
    pub fn cu(&self) -> Cu {
        match self.shape {
            PadShape::Round => Cu::Seg { a: self.at, b: self.at, r: self.size[0].min(self.size[1]) / 2.0 },
            PadShape::Oval => {
                let (w, h) = (self.size[0], self.size[1]);
                let (r, half) = if w > h { (h / 2.0, [(w - h) / 2.0, 0.0]) } else { (w / 2.0, [0.0, (h - w) / 2.0]) };
                let d = rot_deg(half, self.rot);
                Cu::Seg { a: sub(self.at, d), b: add(self.at, d), r }
            }
            PadShape::Rect | PadShape::RoundRect => Cu::Poly(self.corners()),
        }
    }
    pub fn corners(&self) -> Vec<P> {
        let (w, h) = (self.size[0] / 2.0, self.size[1] / 2.0);
        [[-w, -h], [w, -h], [w, h], [-w, h]].iter().map(|c| add(rot_deg(*c, self.rot), self.at)).collect()
    }
}

// ---------------------------------------------------------------- copper geometry

/// A piece of copper: a capsule (track, round pad, via) or a polygon (rectangular pad).
#[derive(Clone, Debug)]
pub enum Cu {
    Seg { a: P, b: P, r: f64 },
    Poly(Vec<P>),
}

impl Cu {
    pub fn bbox(&self) -> (P, P) {
        match self {
            Cu::Seg { a, b, r } => ([a[0].min(b[0]) - r, a[1].min(b[1]) - r], [a[0].max(b[0]) + r, a[1].max(b[1]) + r]),
            Cu::Poly(p) => bbox(p),
        }
    }
    /// Gap between two pieces (0 or less when they touch).
    pub fn gap(&self, o: &Cu) -> f64 {
        match (self, o) {
            (Cu::Seg { a, b, r }, Cu::Seg { a: c, b: d, r: s }) => seg_seg(*a, *b, *c, *d) - r - s,
            (Cu::Seg { a, b, r }, Cu::Poly(p)) | (Cu::Poly(p), Cu::Seg { a, b, r }) => poly_seg(p, *a, *b) - r,
            (Cu::Poly(p), Cu::Poly(q)) => poly_poly(p, q),
        }
    }
    pub fn contains(&self, x: P) -> bool {
        match self {
            Cu::Seg { a, b, r } => seg_point(*a, *b, x) <= *r + 1e-6,
            Cu::Poly(p) => inside(p, x),
        }
    }
    /// As a polygon grown by `d`.
    pub fn polygon(&self, d: f64) -> Contour {
        match self {
            Cu::Seg { a, b, r } => capsule(*a, *b, r + d),
            Cu::Poly(p) => {
                if d.abs() < 1e-9 {
                    p.clone()
                } else {
                    let s = crate::cam_paths::offset(&vec![vec![ccw(p.clone())]], d);
                    s.into_iter().next().and_then(|sh| sh.into_iter().next()).unwrap_or_else(|| p.clone())
                }
            }
        }
    }
}

pub fn ccw(mut c: Contour) -> Contour {
    if crate::cam_paths::area(&c) < 0.0 {
        c.reverse();
    }
    c
}

/// A capsule (stadium) around a segment as a polygon.
pub fn capsule(a: P, b: P, r: f64) -> Contour {
    let n = 12;
    let d = sub(b, a);
    let ang = d[1].atan2(d[0]);
    let mut v = vec![];
    for (c, a0) in [(b, ang - std::f64::consts::FRAC_PI_2), (a, ang + std::f64::consts::FRAC_PI_2)] {
        for i in 0..=n {
            let t = a0 + std::f64::consts::PI * i as f64 / n as f64;
            v.push([c[0] + r * t.cos(), c[1] + r * t.sin()]);
        }
    }
    v
}

pub fn seg_point(a: P, b: P, p: P) -> f64 {
    let d = sub(b, a);
    let l2 = d[0] * d[0] + d[1] * d[1];
    let t = if l2 < 1e-18 { 0.0 } else { (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0) };
    dist(p, [a[0] + d[0] * t, a[1] + d[1] * t])
}

fn cross(o: P, a: P, b: P) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

pub fn segs_cross(a: P, b: P, c: P, d: P) -> bool {
    let (d1, d2, d3, d4) = (cross(c, d, a), cross(c, d, b), cross(a, b, c), cross(a, b, d));
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0)) && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

pub fn seg_seg(a: P, b: P, c: P, d: P) -> f64 {
    if segs_cross(a, b, c, d) {
        return 0.0;
    }
    seg_point(a, b, c).min(seg_point(a, b, d)).min(seg_point(c, d, a)).min(seg_point(c, d, b))
}

pub fn inside(poly: &[P], p: P) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            c = !c;
        }
    }
    c
}

fn edges(p: &[P]) -> impl Iterator<Item = (P, P)> + '_ {
    (0..p.len()).map(move |i| (p[i], p[(i + 1) % p.len()]))
}

fn poly_seg(p: &[P], a: P, b: P) -> f64 {
    if inside(p, a) || inside(p, b) {
        return 0.0;
    }
    edges(p).map(|(c, d)| seg_seg(a, b, c, d)).fold(f64::INFINITY, f64::min)
}

fn poly_poly(p: &[P], q: &[P]) -> f64 {
    if p.iter().any(|x| inside(q, *x)) || q.iter().any(|x| inside(p, *x)) {
        return 0.0;
    }
    edges(p).map(|(a, b)| poly_seg(q, a, b)).fold(f64::INFINITY, f64::min)
}

/// Distance from a point to a closed polygon's boundary.
pub fn outline_dist(poly: &[P], p: P) -> f64 {
    edges(poly).map(|(a, b)| seg_point(a, b, p)).fold(f64::INFINITY, f64::min)
}

// ---------------------------------------------------------------- netlist

#[derive(Clone, Debug, Default)]
pub struct Net {
    pub name: String,
    /// (part index, pin number)
    pub pins: Vec<(usize, String)>,
    /// Named by a power or ground port.
    pub power: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Netlist {
    pub nets: Vec<Net>,
    pub pin_net: HashMap<(usize, String), usize>,
}

impl Netlist {
    pub fn by_name(&self, name: &str) -> Option<usize> {
        self.nets.iter().position(|n| n.name == name)
    }
    pub fn net_of(&self, part: usize, pin: &str) -> Option<usize> {
        self.pin_net.get(&(part, pin.to_string())).copied()
    }
}

struct Dsu(Vec<usize>);

impl Dsu {
    fn new(n: usize) -> Dsu {
        Dsu((0..n).collect())
    }
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

const EPS: f64 = 1e-3;

/// A pin's connection point and the direction its line runs into the symbol.
#[derive(Clone, Debug)]
pub struct SchPin {
    pub part: usize,
    pub num: String,
    pub at: P,
    pub inner: P,
}

impl Pcb {
    pub fn new(name: impl Into<String>) -> Pcb {
        Pcb { name: name.into(), next_id: 1, ..Default::default() }
    }

    pub fn part_index(&self, reference: &str) -> Option<usize> {
        self.parts.iter().position(|p| p.reference.eq_ignore_ascii_case(reference))
    }

    /// Next free reference for a prefix (R1, R2, …).
    pub fn next_reference(&self, prefix: &str) -> String {
        let mut n = 1;
        while self.parts.iter().any(|p| p.reference == format!("{prefix}{n}")) {
            n += 1;
        }
        format!("{prefix}{n}")
    }

    /// Add a part from the library. Its board spot is beside the board until it is placed.
    pub fn add_part(&mut self, key: &str, footprint: Option<&str>, value: Option<&str>, sch: P) -> Result<usize, String> {
        let k = pcb_lib::symbol_kind(key).ok_or_else(|| format!("no symbol {key}; the symbols are {}", pcb_lib::SYMBOLS.iter().map(|s| s.key).collect::<Vec<_>>().join(", ")))?;
        let fp = footprint.unwrap_or(k.footprint);
        if pcb_lib::footprint(fp).is_none() {
            return Err(format!("no footprint {fp}; e.g. {}", pcb_lib::footprint_names().join(", ")));
        }
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let value = value.unwrap_or(k.value);
        let mut p = Part {
            id,
            reference: self.next_reference(k.prefix),
            value: value.to_string(),
            symbol: k.key.to_string(),
            footprint: fp.to_string(),
            pin_names: pcb_lib::default_pin_names(k.key, value),
            sch: snap(sch, pcb_lib::GRID),
            ..Default::default()
        };
        p.at = self.free_spot(&p);
        self.parts.push(p);
        Ok(self.parts.len() - 1)
    }

    /// A spot right of the board where a new part's courtyard is clear of the others.
    pub fn free_spot(&self, p: &Part) -> P {
        let fp = p.footprint();
        let (lo, hi) = self.board.bbox();
        let (cl, ch) = fp.court;
        let mut y = hi[1] - ch[1];
        let mut x = hi[0] + 3.0 - cl[0];
        let boxes: Vec<(P, P)> = self.parts.iter().map(|q| self.court_world(q)).collect();
        for _ in 0..400 {
            let a = [x + cl[0], y + cl[1]];
            let b = [x + ch[0], y + ch[1]];
            if boxes.iter().all(|(l, h)| a[0] > h[0] + 0.5 || b[0] < l[0] - 0.5 || a[1] > h[1] + 0.5 || b[1] < l[1] - 0.5) {
                return snap([x, y], 0.05);
            }
            y -= 1.0;
            if y + cl[1] < lo[1] - 20.0 {
                y = hi[1] - ch[1];
                x += 6.0;
            }
        }
        [x, y]
    }

    /// A part's courtyard on the board, as an axis-aligned box.
    pub fn court_world(&self, p: &Part) -> (P, P) {
        let (a, b) = p.footprint().court;
        bbox(&[p.xf(a), p.xf(b), p.xf([a[0], b[1]]), p.xf([b[0], a[1]])])
    }

    /// Every pin's connection point on the schematic.
    pub fn sch_pins(&self) -> Vec<SchPin> {
        let mut v = vec![];
        for (i, p) in self.parts.iter().enumerate() {
            for sp in p.symbol().pins {
                let inner = [sp.at[0] + sp.dir[0] * sp.len, sp.at[1] + sp.dir[1] * sp.len];
                v.push(SchPin { part: i, num: sp.num, at: p.sch_xf(sp.at), inner: p.sch_xf(inner) });
            }
        }
        v
    }

    /// The nets the schematic makes: pins joined by wires, by touching, or by labels with the same name.
    pub fn netlist(&self) -> Netlist {
        let pins = self.sch_pins();
        let (nw, np, nl) = (self.wires.len(), pins.len(), self.labels.len());
        let mut d = Dsu::new(nw + np + nl);
        let on_wire = |w: &Wire, p: P| seg_point(w.a, w.b, p) < EPS;
        for i in 0..nw {
            for j in i + 1..nw {
                let (a, b) = (&self.wires[i], &self.wires[j]);
                if on_wire(a, b.a) || on_wire(a, b.b) || on_wire(b, a.a) || on_wire(b, a.b) {
                    d.join(i, j);
                }
            }
        }
        for (k, p) in pins.iter().enumerate() {
            for (i, w) in self.wires.iter().enumerate() {
                if on_wire(w, p.at) {
                    d.join(nw + k, i);
                }
            }
            for (j, q) in pins.iter().enumerate().skip(k + 1) {
                if dist(p.at, q.at) < EPS {
                    d.join(nw + k, nw + j);
                }
            }
        }
        for (k, l) in self.labels.iter().enumerate() {
            for (i, w) in self.wires.iter().enumerate() {
                if on_wire(w, l.at) {
                    d.join(nw + np + k, i);
                }
            }
            for (j, p) in pins.iter().enumerate() {
                if dist(l.at, p.at) < EPS {
                    d.join(nw + np + k, nw + j);
                }
            }
            for (j, m) in self.labels.iter().enumerate().skip(k + 1) {
                // labels name nets by their trimmed text; blank ones join nothing
                if !l.text.trim().is_empty() && m.text.trim() == l.text.trim() {
                    d.join(nw + np + k, nw + np + j);
                }
            }
        }
        // groups that hold at least one pin
        let mut groups: Vec<(usize, Net)> = vec![];
        let mut out = Netlist::default();
        for (k, p) in pins.iter().enumerate() {
            let root = d.find(nw + k);
            let gi = match groups.iter().position(|g| g.0 == root) {
                Some(g) => g,
                None => {
                    groups.push((root, Net::default()));
                    groups.len() - 1
                }
            };
            groups[gi].1.pins.push((p.part, p.num.clone()));
        }
        for (k, l) in self.labels.iter().enumerate() {
            let root = d.find(nw + np + k);
            if let Some(g) = groups.iter_mut().find(|g| g.0 == root) {
                let better = g.1.name.is_empty() || (l.kind != LabelKind::Net && !g.1.power);
                if better && !l.text.trim().is_empty() {
                    g.1.name = l.text.trim().to_string();
                    g.1.power = l.kind != LabelKind::Net;
                }
            }
        }
        for (_, mut n) in groups {
            if n.pins.len() < 2 && n.name.is_empty() {
                // an unconnected pin
                continue;
            }
            n.pins.sort_by(|a, b| natural(&self.parts[a.0].reference, &self.parts[b.0].reference).then(natural(&a.1, &b.1)));
            if n.name.is_empty() {
                let (pi, num) = &n.pins[0];
                n.name = format!("Net-({}-{})", self.parts[*pi].reference, num);
            }
            let ni = out.nets.len();
            for p in &n.pins {
                out.pin_net.insert(p.clone(), ni);
            }
            out.nets.push(n);
        }
        out.nets.sort_by(|a, b| natural(&a.name, &b.name));
        out.pin_net.clear();
        for (ni, n) in out.nets.iter().enumerate() {
            for p in &n.pins {
                out.pin_net.insert(p.clone(), ni);
            }
        }
        out
    }

    /// Every pad on the board with its net.
    pub fn pads(&self, nl: &Netlist) -> Vec<WPad> {
        let mut v = vec![];
        for (i, p) in self.parts.iter().enumerate() {
            let fp = p.footprint();
            for pad in &fp.pads {
                v.push(WPad {
                    part: i,
                    num: pad.num.clone(),
                    at: p.xf(pad.at),
                    size: pad.size,
                    shape: pad.shape,
                    rot: p.rot,
                    drill: pad.drill,
                    layer: if pad.drill > 0.0 { None } else { Some(p.side()) },
                    net: nl.net_of(i, &pad.num),
                });
            }
        }
        v
    }

    /// Give copper whose net name the schematic no longer has (an unnamed net whose generated
    /// name changed, a relabelled net) the name of the one net whose pads it touches. Copper that
    /// touches no pad, or pads of several nets, keeps its name. Returns whether anything changed.
    pub fn retag_copper(&mut self, nl: &Netlist) -> bool {
        let known = |n: &str| n.is_empty() || nl.by_name(n).is_some();
        let b = &self.board;
        let mut stale: Vec<String> = b.tracks.iter().map(|t| &t.net).chain(b.vias.iter().map(|v| &v.net)).filter(|n| !known(n)).cloned().collect();
        stale.sort();
        stale.dedup();
        if stale.is_empty() {
            return false;
        }
        let pads = self.pads(nl);
        let its = items(self, nl, &pads, &[]);
        let mut map: HashMap<String, String> = HashMap::new();
        for s in stale {
            let mut nets: Vec<usize> = vec![];
            for it in its.iter().filter(|it| it.net == s && matches!(it.kind, ItemKind::Track(_) | ItemKind::Via(_))) {
                for pad in its.iter().filter(|x| matches!(x.kind, ItemKind::Pad(_))) {
                    let ItemKind::Pad(pi) = pad.kind else { continue };
                    if let Some(n) = pads[pi].net {
                        if !nets.contains(&n) && it.shares_layer(pad) && it.cu.gap(&pad.cu) <= EPS {
                            nets.push(n);
                        }
                    }
                }
            }
            if let [n] = nets[..] {
                map.insert(s, nl.nets[n].name.clone());
            }
        }
        if map.is_empty() {
            return false;
        }
        let b = &mut self.board;
        for n in b.tracks.iter_mut().map(|t| &mut t.net).chain(b.vias.iter_mut().map(|v| &mut v.net)).chain(b.zones.iter_mut().map(|z| &mut z.net)) {
            if let Some(m) = map.get(n.as_str()) {
                *n = m.clone();
            }
        }
        true
    }

    /// The width a new track on `net` gets.
    pub fn track_width(&self, nl: &Netlist, net: &str) -> f64 {
        self.net_rules(nl, net).track
    }
    /// The rules for a net: its class's, or the board's.
    pub fn net_rules(&self, nl: &Netlist, net: &str) -> NetRules {
        let power = nl.by_name(net).is_some_and(|n| nl.nets[n].power);
        self.board.rules.for_net(net, power)
    }
}

/// Snap to a grid.
pub fn snap(p: P, g: f64) -> P {
    [(p[0] / g).round() * g, (p[1] / g).round() * g]
}

/// Compare with runs of digits as numbers (R2 before R10).
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| -> Vec<(String, u64)> {
        let mut out = vec![];
        let mut text = String::new();
        let mut num = String::new();
        for c in s.chars() {
            if c.is_ascii_digit() {
                num.push(c);
            } else {
                if !num.is_empty() {
                    out.push((std::mem::take(&mut text), num.parse().unwrap_or(0)));
                    num.clear();
                }
                text.push(c);
            }
        }
        out.push((text, num.parse().unwrap_or(0)));
        out
    };
    key(a).cmp(&key(b))
}

// ---------------------------------------------------------------- board connectivity

/// A copper item on the board, for connectivity and checks.
#[derive(Clone, Debug)]
pub struct Item {
    pub kind: ItemKind,
    pub cu: Cu,
    /// None: on both layers.
    pub layer: Option<Layer>,
    /// Net name; "" for a pad with no connection.
    pub net: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Pad(usize),
    Track(usize),
    Via(usize),
    /// A zone's fill piece (zone index, piece index).
    Fill(usize, usize),
}

impl Item {
    pub fn shares_layer(&self, o: &Item) -> bool {
        match (self.layer, o.layer) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
    }
}

/// A filled zone: one or more pieces, each an outer contour and holes.
pub type Fill = Shapes;

/// Everything copper: pads, tracks, vias and zone fills.
pub fn items(pcb: &Pcb, nl: &Netlist, pads: &[WPad], fills: &[Fill]) -> Vec<Item> {
    let mut v = vec![];
    for (i, p) in pads.iter().enumerate() {
        v.push(Item { kind: ItemKind::Pad(i), cu: p.cu(), layer: p.layer, net: p.net.map(|n| nl.nets[n].name.clone()).unwrap_or_default() });
    }
    for (i, t) in pcb.board.tracks.iter().enumerate() {
        v.push(Item { kind: ItemKind::Track(i), cu: Cu::Seg { a: t.a, b: t.b, r: t.width / 2.0 }, layer: Some(t.layer), net: t.net.clone() });
    }
    for (i, t) in pcb.board.vias.iter().enumerate() {
        v.push(Item { kind: ItemKind::Via(i), cu: Cu::Seg { a: t.at, b: t.at, r: t.d / 2.0 }, layer: None, net: t.net.clone() });
    }
    for (zi, f) in fills.iter().enumerate() {
        let Some(z) = pcb.board.zones.get(zi) else { continue };
        for (k, piece) in f.iter().enumerate() {
            if let Some(outer) = piece.first() {
                v.push(Item { kind: ItemKind::Fill(zi, k), cu: Cu::Poly(outer.clone()), layer: Some(z.layer), net: z.net.clone() });
            }
        }
    }
    v
}

fn touching(a: &Item, b: &Item, fills: &[Fill]) -> bool {
    if !a.shares_layer(b) {
        return false;
    }
    let (la, ha) = a.cu.bbox();
    let (lb, hb) = b.cu.bbox();
    if la[0] > hb[0] + EPS || lb[0] > ha[0] + EPS || la[1] > hb[1] + EPS || lb[1] > ha[1] + EPS {
        return false;
    }
    // a fill touches what lies on its copper, not in its holes
    let in_fill = |f: &Item, o: &Item| -> bool {
        let ItemKind::Fill(z, k) = f.kind else { return false };
        let Some(piece) = fills.get(z).and_then(|x| x.get(k)) else { return false };
        probe(&o.cu).iter().any(|p| inside(&piece[0], *p) && !piece[1..].iter().any(|h| inside(h, *p)))
    };
    match (a.kind, b.kind) {
        (ItemKind::Fill(..), ItemKind::Fill(..)) => false,
        (ItemKind::Fill(..), _) => in_fill(a, b),
        (_, ItemKind::Fill(..)) => in_fill(b, a),
        _ => a.cu.gap(&b.cu) <= EPS,
    }
}

/// An unrouted connection.
#[derive(Clone, Debug)]
pub struct Airwire {
    pub a: P,
    pub b: P,
    pub net: String,
}

/// Group copper items of each net into connected islands and join the islands with the
/// shortest airwires (the ratsnest).
pub fn ratsnest(its: &[Item], pads: &[WPad], fills: &[Fill]) -> Vec<Airwire> {
    let mut by_net: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, it) in its.iter().enumerate() {
        if !it.net.is_empty() {
            by_net.entry(it.net.as_str()).or_default().push(i);
        }
    }
    let mut out = vec![];
    let mut nets: Vec<&&str> = by_net.keys().collect();
    nets.sort();
    for net in nets {
        let ids = &by_net[*net];
        let mut d = Dsu::new(ids.len());
        for a in 0..ids.len() {
            for b in a + 1..ids.len() {
                if touching(&its[ids[a]], &its[ids[b]], fills) {
                    d.join(a, b);
                }
            }
        }
        // islands that hold pads, with their anchor points
        let mut islands: Vec<(usize, Vec<P>)> = vec![];
        for (k, &i) in ids.iter().enumerate() {
            let ItemKind::Pad(pi) = its[i].kind else { continue };
            let r = d.find(k);
            match islands.iter_mut().find(|x| x.0 == r) {
                Some(x) => x.1.push(pads[pi].at),
                None => islands.push((r, vec![pads[pi].at])),
            }
        }
        // tracks and vias in a pad island are anchors too, so airwires leave from the track's end
        for (k, &i) in ids.iter().enumerate() {
            let pts = match (&its[i].kind, &its[i].cu) {
                (ItemKind::Track(_), Cu::Seg { a, b, .. }) => vec![*a, *b],
                (ItemKind::Via(_), Cu::Seg { a, .. }) => vec![*a],
                _ => continue,
            };
            let r = d.find(k);
            if let Some(x) = islands.iter_mut().find(|x| x.0 == r) {
                x.1.extend(pts);
            }
        }
        if islands.len() < 2 {
            continue;
        }
        // Prim's algorithm over islands
        let n = islands.len();
        let mut done = vec![false; n];
        done[0] = true;
        for _ in 1..n {
            let mut best: Option<(f64, usize, P, P)> = None;
            for a in (0..n).filter(|&a| done[a]) {
                for b in (0..n).filter(|&b| !done[b]) {
                    for p in &islands[a].1 {
                        for q in &islands[b].1 {
                            let l = dist(*p, *q);
                            if best.as_ref().is_none_or(|x| l < x.0) {
                                best = Some((l, b, *p, *q));
                            }
                        }
                    }
                }
            }
            let Some((_, b, p, q)) = best else { break };
            done[b] = true;
            out.push(Airwire { a: p, b: q, net: net.to_string() });
        }
    }
    out
}

// ---------------------------------------------------------------- zone fill

fn overlay(a: &Shapes, b: &Shapes, rule: i_overlay::core::overlay_rule::OverlayRule) -> Shapes {
    use i_overlay::core::fill_rule::FillRule;
    use i_overlay::float::single::SingleFloatOverlay;
    let fa = crate::cam_paths::flat(a);
    let fb = crate::cam_paths::flat(b);
    if fa.is_empty() {
        return vec![];
    }
    fa.overlay(&fb, rule, FillRule::NonZero)
}

/// Whether a zone joins a pad with a thermal relief.
pub fn relieved(z: &Zone, pad: &WPad) -> bool {
    match z.pads {
        PadConnect::Solid => false,
        PadConnect::Relief => true,
        PadConnect::ThtRelief => pad.drill > 0.0,
    }
}

/// A zone's thermal relief gap and spoke width for its net.
pub fn relief_sizes(pcb: &Pcb, nl: &Netlist, z: &Zone) -> (f64, f64) {
    let r = pcb.net_rules(nl, &z.net);
    let gap = if z.relief_gap > 0.0 { z.relief_gap } else { r.clearance.max(0.3) };
    let spoke = if z.spoke > 0.0 { z.spoke } else { r.track.max(pcb.board.rules.min_track) };
    (gap, spoke)
}

/// The four spokes of a pad's thermal relief, out to `reach` past its edge, along the pad's axes.
pub fn spokes(pad: &WPad, reach: f64, w: f64) -> Vec<Contour> {
    let (hx, hy) = (pad.size[0] / 2.0 + reach, pad.size[1] / 2.0 + reach);
    let (s, c) = pad.rot.to_radians().sin_cos();
    let dir = |d: P| [d[0] * c - d[1] * s, d[0] * s + d[1] * c];
    [[hx, 0.0], [-hx, 0.0], [0.0, hy], [0.0, -hy]].iter().map(|e| { let e = dir(*e); capsule(pad.at, add(pad.at, e), w / 2.0) }).collect()
}

/// Fill every zone: its outline inside the board, less other nets' copper grown by the clearance,
/// less holes and keepouts; pads of its own net are joined solid or with thermal reliefs. Pieces
/// that don't touch their own net's copper are dropped.
pub fn fill_zones(pcb: &Pcb, nl: &Netlist, pads: &[WPad]) -> Vec<Fill> {
    use i_overlay::core::overlay_rule::OverlayRule;
    let b = &pcb.board;
    let r = &b.rules;
    let board = crate::cam_paths::offset(&vec![vec![ccw(b.outline.clone())]], -r.edge);
    let base_items = items(pcb, nl, pads, &[]);
    let mut cl_cache: HashMap<String, f64> = HashMap::new();
    let mut clear = |net: &str| -> f64 { *cl_cache.entry(net.to_string()).or_insert_with(|| pcb.net_rules(nl, net).clearance) };
    let mut fills: Vec<Fill> = vec![];
    for (zi, z) in b.zones.iter().enumerate() {
        if z.outline.len() < 3 {
            fills.push(vec![]);
            continue;
        }
        let zc = if z.clearance > 0.0 { z.clearance } else { clear(&z.net) };
        let area0 = overlay(&vec![vec![ccw(z.outline.clone())]], &board, OverlayRule::Intersect);
        // keep clear of other nets, and of earlier zones on the same layer
        let mut obstacles: Vec<Contour> = vec![];
        for it in &base_items {
            if it.layer.is_some_and(|l| l != z.layer) {
                continue;
            }
            if it.net != z.net || it.net.is_empty() {
                let c = if it.net.is_empty() { zc } else { zc.max(clear(&it.net)) };
                obstacles.push(it.cu.polygon(c));
            }
        }
        for h in &b.holes {
            obstacles.push(capsule(h.at, h.at, h.d / 2.0 + zc));
        }
        for k in b.keepouts.iter().filter(|k| k.pours && k.on(Some(z.layer)) && k.outline.len() >= 3) {
            obstacles.push(ccw(k.outline.clone()));
        }
        for (k, other) in b.zones.iter().enumerate().take(zi) {
            if other.layer == z.layer && other.net != z.net {
                for piece in &fills[k] {
                    let grown = crate::cam_paths::offset(&vec![piece.clone()], zc);
                    obstacles.extend(grown.into_iter().flatten());
                }
            }
        }
        // thermal reliefs: a ring cleared round the pad, then spokes across it
        let (gap, spoke) = relief_sizes(pcb, nl, z);
        let mut reliefs: Vec<&WPad> = vec![];
        if !z.net.is_empty() {
            for p in pads.iter().filter(|p| p.on(z.layer) && p.net.is_some_and(|n| nl.nets[n].name == z.net) && relieved(z, p)) {
                obstacles.push(p.cu().polygon(gap));
                reliefs.push(p);
            }
        }
        let mut area = if obstacles.is_empty() { area0.clone() } else { overlay(&area0, &vec![obstacles.clone()], OverlayRule::Difference) };
        // drop slivers narrower than the minimum track (before the spokes, which may be that thin)
        let h = r.min_track / 2.0;
        area = crate::cam_paths::offset(&crate::cam_paths::offset(&area, -h), h);
        if !reliefs.is_empty() {
            let mut sp: Vec<Contour> = vec![];
            for p in &reliefs {
                sp.extend(spokes(p, gap + spoke, spoke));
            }
            // spokes may not cross other nets' clearance
            let others: Vec<Contour> = obstacles.iter().take(obstacles.len() - reliefs.len()).cloned().collect();
            let mut sp: Shapes = overlay(&vec![sp], &area0, OverlayRule::Intersect);
            if !others.is_empty() {
                sp = overlay(&sp, &vec![others], OverlayRule::Difference);
            }
            area = overlay(&area, &sp, OverlayRule::Union);
        }
        // keep pieces that reach a pad, via or track of the net
        let own: Vec<&Item> = base_items.iter().filter(|it| it.net == z.net && !z.net.is_empty() && it.layer.is_none_or(|l| l == z.layer)).collect();
        area.retain(|piece| own.iter().any(|it| probe(&it.cu).iter().any(|p| inside(&piece[0], *p) && !piece[1..].iter().any(|h| inside(h, *p)))));
        fills.push(area);
    }
    fills
}

/// Points of a piece of copper that a pour covering it covers: its ends or corners and its centre
/// (a relieved pad's corners sit in the relief gap, its centre under the spokes).
fn probe(cu: &Cu) -> Vec<P> {
    match cu {
        Cu::Seg { a, b, .. } => vec![*a, *b],
        Cu::Poly(p) => {
            let mut v = p.clone();
            v.push(centre(cu));
            v
        }
    }
}

// ---------------------------------------------------------------- design rule check

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub at: P,
    pub message: String,
    /// Errors stop a board from being made right; warnings are worth a look.
    pub error: bool,
    /// The check that found it (an id in `CHECKS`).
    pub rule: &'static str,
}

impl Issue {
    pub fn new(rule: &'static str, at: P, error: bool, message: String) -> Issue {
        Issue { at, message, error, rule }
    }
}

fn item_label(pcb: &Pcb, pads: &[WPad], it: &Item) -> String {
    match it.kind {
        ItemKind::Pad(i) => format!("{} pad {}", pcb.parts[pads[i].part].reference, pads[i].num),
        ItemKind::Track(_) => format!("track ({})", if it.net.is_empty() { "no net" } else { &it.net }),
        ItemKind::Via(_) => format!("via ({})", if it.net.is_empty() { "no net" } else { &it.net }),
        ItemKind::Fill(..) => format!("{} pour", it.net),
    }
}

fn centre(cu: &Cu) -> P {
    let (a, b) = cu.bbox();
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

/// A part's 3D bodies in board space: the footprint in plan, and its height range with z = 0 on
/// the board's bottom face (a top part sits on z = thickness, a bottom part hangs below 0).
pub fn bodies(pcb: &Pcb, part: &Part) -> Vec<(Cu, f64, f64)> {
    let t = pcb.board.thickness;
    let z = |lo: f64, hi: f64| if part.bottom { (-hi, -lo) } else { (t + lo, t + hi) };
    part.footprint()
        .body
        .iter()
        .map(|b| match b {
            pcb_lib::Solid::Box { lo, hi } => {
                let c = [[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]].map(|q| part.xf(q));
                let (z0, z1) = z(lo[2], hi[2]);
                (Cu::Poly(c.to_vec()), z0, z1)
            }
            pcb_lib::Solid::Cyl { c, r, z0, h } => {
                let at = part.xf(*c);
                let (a, b) = z(*z0, z0 + h);
                (Cu::Seg { a: at, b: at, r: *r }, a, b)
            }
        })
        .collect()
}

/// How far a part reaches above its side of the board (its tallest body).
pub fn part_height(pcb: &Pcb, part: &Part) -> f64 {
    let t = pcb.board.thickness;
    bodies(pcb, part).iter().map(|(_, z0, z1)| if part.bottom { -z0 } else { z1 - t }).fold(0.0, f64::max)
}

/// Check the board against its rules: clearances (per net class), shorts, widths, drills, hole
/// spacing, the board edge, keepouts, silkscreen, overlapping courtyards and 3D bodies, heights,
/// dangling tracks, starved thermals and unrouted connections. Checks in `rules.off` are skipped.
pub fn drc(pcb: &Pcb, nl: &Netlist, pads: &[WPad], fills: &[Fill]) -> Vec<Issue> {
    let b = &pcb.board;
    let r = &b.rules;
    let its = items(pcb, nl, pads, &[]);
    let mut out = vec![];
    let mut rules_cache: HashMap<&str, NetRules> = HashMap::new();
    for it in &its {
        rules_cache.entry(it.net.as_str()).or_insert_with(|| pcb.net_rules(nl, &it.net));
    }
    let clr = |net: &str| rules_cache.get(net).map(|x| x.clearance).unwrap_or(r.clearance);
    let max_cl = rules_cache.values().map(|x| x.clearance).fold(r.clearance, f64::max);
    // pads with no net are each their own net
    let net_key = |i: usize| -> String { if its[i].net.is_empty() { format!("\u{0}{i}") } else { its[i].net.clone() } };
    let bbs: Vec<(P, P)> = its.iter().map(|i| i.cu.bbox()).collect();
    for i in 0..its.len() {
        for j in i + 1..its.len() {
            if !its[i].shares_layer(&its[j]) || net_key(i) == net_key(j) {
                continue;
            }
            // pads of one part with no nets don't matter to each other
            if let (ItemKind::Pad(a), ItemKind::Pad(b)) = (its[i].kind, its[j].kind) {
                if pads[a].part == pads[b].part && (its[i].net.is_empty() || its[j].net.is_empty()) {
                    continue;
                }
            }
            let (la, ha) = bbs[i];
            let (lb, hb) = bbs[j];
            let m = max_cl;
            if la[0] > hb[0] + m || lb[0] > ha[0] + m || la[1] > hb[1] + m || lb[1] > ha[1] + m {
                continue;
            }
            // a footprint's own pads are as far apart as the part makes them: only the board rule
            let same_part = matches!((its[i].kind, its[j].kind), (ItemKind::Pad(a), ItemKind::Pad(b)) if pads[a].part == pads[b].part);
            let need = if same_part { r.clearance } else { clr(&its[i].net).max(clr(&its[j].net)) };
            let g = its[i].cu.gap(&its[j].cu);
            if g < need - 1e-4 {
                let (a, bb) = (item_label(pcb, pads, &its[i]), item_label(pcb, pads, &its[j]));
                let at = if its[i].cu.contains(centre(&its[j].cu)) { centre(&its[j].cu) } else { centre(&its[i].cu) };
                if g <= 1e-6 && !its[i].net.is_empty() && !its[j].net.is_empty() {
                    out.push(Issue::new("short", at, true, format!("Short: {a} touches {bb}")));
                } else {
                    out.push(Issue::new("clearance", at, true, format!("Clearance {:.3} mm < {} mm: {a} and {bb}", g.max(0.0), need)));
                }
            }
        }
    }
    for t in &b.tracks {
        if t.width < r.min_track - 1e-9 {
            out.push(Issue::new("width", t.a, true, format!("Track {} mm wide is under the {} mm minimum", t.width, r.min_track)));
        } else if let Some(c) = r.class_of(&t.net) {
            let want = pcb.net_rules(nl, &t.net).track;
            if t.width < want - 1e-6 {
                out.push(Issue::new("class_width", closest_mid(t), false, format!("Track on {} is {} mm wide; net class {} wants {} mm", t.net, t.width, c.name, want)));
            }
        }
    }
    for v in &b.vias {
        if v.drill < r.min_drill - 1e-9 {
            out.push(Issue::new("via", v.at, true, format!("Via drill {} mm is under the {} mm minimum", v.drill, r.min_drill)));
        }
        if (v.d - v.drill) / 2.0 < r.min_annular - 1e-9 {
            out.push(Issue::new("via", v.at, true, format!("Via annular ring {:.3} mm is under {} mm", (v.d - v.drill) / 2.0, r.min_annular)));
        }
    }
    // holes too close together
    let mut drills: Vec<(P, f64, String)> = vec![];
    for p in pads.iter().filter(|p| p.drill > 0.0) {
        drills.push((p.at, p.drill, format!("{} pad {}", pcb.parts[p.part].reference, p.num)));
    }
    for v in &b.vias {
        drills.push((v.at, v.drill, format!("via ({})", if v.net.is_empty() { "no net" } else { &v.net })));
    }
    for h in &b.holes {
        drills.push((h.at, h.d, "mounting hole".into()));
    }
    for i in 0..drills.len() {
        for j in i + 1..drills.len() {
            let c = dist(drills[i].0, drills[j].0);
            if c < 1e-6 {
                continue;
            }
            let g = c - (drills[i].1 + drills[j].1) / 2.0;
            if g < r.hole_gap - 1e-4 {
                out.push(Issue::new("hole_gap", drills[i].0, true, format!("Holes {:.3} mm apart (rule {} mm): {} and {}", g.max(0.0), r.hole_gap, drills[i].2, drills[j].2)));
            }
        }
    }
    // copper near or past the edge
    if b.outline.len() >= 3 {
        for it in &its {
            let pts: Vec<P> = match &it.cu {
                Cu::Seg { a, b, .. } => vec![*a, *b],
                Cu::Poly(p) => p.clone(),
            };
            let rad = match &it.cu {
                Cu::Seg { r, .. } => *r,
                Cu::Poly(_) => 0.0,
            };
            let outside = pts.iter().any(|p| !inside(&b.outline, *p));
            let near = match &it.cu {
                Cu::Seg { a, b: bb, .. } => edges(&b.outline).map(|(c, d)| seg_seg(*a, *bb, c, d)).fold(f64::INFINITY, f64::min) - rad,
                Cu::Poly(p) => edges(&b.outline).map(|(c, d)| poly_seg(p, c, d)).fold(f64::INFINITY, f64::min),
            };
            if outside {
                out.push(Issue::new("edge", centre(&it.cu), true, format!("{} is off the board", item_label(pcb, pads, it))));
            } else if near < r.edge - 1e-4 {
                out.push(Issue::new("edge", centre(&it.cu), true, format!("{} is {:.2} mm from the board edge (rule {} mm)", item_label(pcb, pads, it), near.max(0.0), r.edge)));
            }
        }
    }
    // keepouts
    for (ki, k) in b.keepouts.iter().enumerate() {
        if k.outline.len() < 3 {
            continue;
        }
        let area = Cu::Poly(k.outline.clone());
        let name = format!("keepout {}", ki + 1);
        for it in &its {
            let hit = match it.kind {
                ItemKind::Track(_) => k.tracks && k.on(it.layer),
                ItemKind::Via(_) => k.vias,
                _ => false,
            };
            if hit && it.cu.gap(&area) <= 1e-6 {
                out.push(Issue::new("keepout", centre(&it.cu), true, format!("{} is in {name}", item_label(pcb, pads, it))));
            }
        }
        if k.parts {
            for p in &pcb.parts {
                if k.on(Some(p.side())) {
                    let (lo, hi) = pcb.court_world(p);
                    let court = Cu::Poly(vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]);
                    if court.gap(&area) <= 1e-6 {
                        out.push(Issue::new("keepout", p.at, true, format!("{} is in {name}", p.reference)));
                    }
                }
            }
        }
    }
    // silkscreen over exposed copper (the mask opens 0.05 mm round each pad)
    if r.checks("silk") {
        for (pi, part) in pcb.parts.iter().enumerate() {
            let mut list = vec![];
            crate::pcb_export::silk(&mut list, part, [0; 4]);
            let side = part.side();
            let mut hits: Vec<String> = vec![];
            let mut at = part.at;
            for d in &list {
                let crate::pcb_export::Prim::Seg { a, b: bb, w } = d.prim else { continue };
                let seg = Cu::Seg { a, b: bb, r: w / 2.0 };
                for pad in pads.iter().filter(|x| x.on(side)) {
                    let who = format!("{} pad {}", pcb.parts[pad.part].reference, pad.num);
                    if !hits.contains(&who) && seg.gap(&pad.cu()) < r.silk_gap + 0.05 - 1e-6 {
                        if hits.is_empty() {
                            at = pad.at;
                        }
                        hits.push(who);
                    }
                }
            }
            if !hits.is_empty() {
                let more = if hits.len() > 3 { format!(" and {} more", hits.len() - 3) } else { String::new() };
                hits.truncate(3);
                out.push(Issue::new("silk", at, false, format!("{} silkscreen is over {}{more}", pcb.parts[pi].reference, hits.join(", "))));
            }
        }
    }
    // overlapping parts
    let courts: Vec<(P, P)> = pcb.parts.iter().map(|p| pcb.court_world(p)).collect();
    for i in 0..pcb.parts.len() {
        for j in i + 1..pcb.parts.len() {
            let both_tht = pcb.parts[i].footprint().tht() && pcb.parts[j].footprint().tht();
            if pcb.parts[i].bottom != pcb.parts[j].bottom && !both_tht {
                continue;
            }
            let (a, b2) = (courts[i], courts[j]);
            if a.0[0] < b2.1[0] - 0.05 && b2.0[0] < a.1[0] - 0.05 && a.0[1] < b2.1[1] - 0.05 && b2.0[1] < a.1[1] - 0.05 {
                let at = [(a.0[0].max(b2.0[0]) + a.1[0].min(b2.1[0])) / 2.0, (a.0[1].max(b2.0[1]) + a.1[1].min(b2.1[1])) / 2.0];
                out.push(Issue::new("overlap", at, false, format!("{} and {} overlap", pcb.parts[i].reference, pcb.parts[j].reference)));
            }
        }
    }
    // 3D bodies that collide (a through-hole part's leads with a bottom part, two tall parts)
    if r.checks("body") {
        let bs: Vec<Vec<(Cu, f64, f64)>> = pcb.parts.iter().map(|p| bodies(pcb, p)).collect();
        for i in 0..bs.len() {
            for j in i + 1..bs.len() {
                let mut hit: Option<(P, f64)> = None;
                for (ca, a0, a1) in &bs[i] {
                    for (cb, b0, b1) in &bs[j] {
                        let zgap = (b0 - a1).max(a0 - b1);
                        let g = ca.gap(cb);
                        // with no gap rule, bodies must overlap (in plan and in height) to collide
                        let near = if r.body_gap > 0.0 { g < r.body_gap - 1e-6 && zgap < r.body_gap - 1e-6 } else { g <= 1e-9 && zgap < -1e-6 };
                        if near {
                            let at = if ca.contains(centre(cb)) { centre(cb) } else { centre(ca) };
                            hit = Some((at, g.max(0.0)));
                        }
                    }
                }
                if let Some((at, g)) = hit {
                    let msg = if g <= 0.0 { format!("{} and {} collide in 3D", pcb.parts[i].reference, pcb.parts[j].reference) } else { format!("{} and {} bodies are {:.2} mm apart (rule {} mm)", pcb.parts[i].reference, pcb.parts[j].reference, g, r.body_gap) };
                    out.push(Issue::new("body", at, true, msg));
                }
            }
        }
    }
    if r.max_height > 0.0 {
        for p in &pcb.parts {
            let h = part_height(pcb, p);
            if h > r.max_height + 1e-6 {
                out.push(Issue::new("height", p.at, true, format!("{} is {:.1} mm tall; the limit is {} mm", p.reference, h, r.max_height)));
            }
        }
    }
    for (i, p) in pcb.parts.iter().enumerate() {
        let (lo, hi) = courts[i];
        if b.outline.len() >= 3 && [lo, hi, [lo[0], hi[1]], [hi[0], lo[1]]].iter().any(|c| !inside(&b.outline, *c)) {
            out.push(Issue::new("outside", p.at, true, format!("{} is not inside the board", p.reference)));
        }
    }
    let all = items(pcb, nl, pads, fills);
    // track ends that touch nothing (antennas)
    if r.checks("antenna") {
        for (ti, t) in b.tracks.iter().enumerate() {
            for end in [t.a, t.b] {
                let tip = Item { kind: ItemKind::Track(ti), cu: Cu::Seg { a: end, b: end, r: t.width / 2.0 }, layer: Some(t.layer), net: t.net.clone() };
                let joined = all.iter().any(|o| o.kind != ItemKind::Track(ti) && (o.net == t.net || o.net.is_empty() || t.net.is_empty()) && touching(&tip, o, fills));
                if !joined {
                    out.push(Issue::new("antenna", end, false, format!("Track on {} ends on nothing", if t.net.is_empty() { "no net" } else { &t.net })));
                }
            }
        }
    }
    // thermal reliefs that too few spokes reach
    if r.checks("thermal") {
        for (zi, z) in b.zones.iter().enumerate() {
            let Some(fill) = fills.get(zi) else { continue };
            if fill.is_empty() {
                continue;
            }
            let (gap, spoke) = relief_sizes(pcb, nl, z);
            for p in pads.iter().filter(|p| p.on(z.layer) && p.net.is_some_and(|n| nl.nets[n].name == z.net) && relieved(z, p)) {
                let (s, c) = p.rot.to_radians().sin_cos();
                let tips = [[p.size[0] / 2.0, 0.0], [-p.size[0] / 2.0, 0.0], [0.0, p.size[1] / 2.0], [0.0, -p.size[1] / 2.0]].map(|e| {
                    let l = (e[0].hypot(e[1])).max(1e-9);
                    let e = [e[0] * (1.0 + (gap + spoke * 0.75) / l), e[1] * (1.0 + (gap + spoke * 0.75) / l)];
                    add(p.at, [e[0] * c - e[1] * s, e[0] * s + e[1] * c])
                });
                let n = tips.iter().filter(|q| fill.iter().any(|piece| inside(&piece[0], **q) && !piece[1..].iter().any(|h| inside(h, **q)))).count();
                if n < 2 {
                    out.push(Issue::new("thermal", p.at, false, format!("{} pad {}: only {n} of 4 thermal spokes reach the {} pour", pcb.parts[p.part].reference, p.num, z.net)));
                }
            }
        }
    }
    out.extend(crate::pcb_tune::length_issues(pcb, nl));
    let air = ratsnest(&all, pads, fills);
    for a in &air {
        out.push(Issue::new("unrouted", [(a.a[0] + a.b[0]) / 2.0, (a.a[1] + a.b[1]) / 2.0], true, format!("Unrouted: {}", a.net)));
    }
    out.retain(|i| r.checks(i.rule));
    out
}

fn closest_mid(t: &Track) -> P {
    [(t.a[0] + t.b[0]) / 2.0, (t.a[1] + t.b[1]) / 2.0]
}

/// Which net a point on a layer belongs to: a pad, track or via under it.
pub fn net_at(pcb: &Pcb, nl: &Netlist, pads: &[WPad], at: P, layer: Layer) -> Option<(String, P)> {
    for p in pads {
        if p.on(layer) && p.cu().contains(at) {
            return Some((p.net.map(|n| nl.nets[n].name.clone()).unwrap_or_default(), p.at));
        }
    }
    for v in &pcb.board.vias {
        if dist(v.at, at) <= v.d / 2.0 {
            return Some((v.net.clone(), v.at));
        }
    }
    for t in &pcb.board.tracks {
        if t.layer == layer && seg_point(t.a, t.b, at) <= t.width / 2.0 {
            let end = if dist(t.a, at) < dist(t.b, at) { t.a } else { t.b };
            let snap_to = if dist(end, at) < t.width * 2.0 { end } else { closest_on(t.a, t.b, at) };
            return Some((t.net.clone(), snap_to));
        }
    }
    None
}

pub fn closest_on(a: P, b: P, p: P) -> P {
    let d = sub(b, a);
    let l2 = d[0] * d[0] + d[1] * d[1];
    let t = if l2 < 1e-18 { 0.0 } else { (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0) };
    [a[0] + d[0] * t, a[1] + d[1] * t]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An LED with its resistor: two parts, wires, a supply and ground port.
    pub(crate) fn led_board() -> Pcb {
        let mut p = Pcb::new("LED");
        let r = p.add_part("R", None, Some("330"), [0.0, 0.0]).unwrap();
        let d = p.add_part("LED", None, None, [10.16, -15.24]).unwrap();
        let j = p.add_part("J", Some("PinHeader_1x02"), None, [-15.24, 0.0]).unwrap();
        // R pin 2 (bottom) down and across to the LED anode (top)
        let r2 = p.parts[r].sch_xf([0.0, -5.08]);
        let da = p.parts[d].sch_xf([0.0, 5.08]);
        p.wires.push(Wire { a: r2, b: [r2[0], da[1]] });
        p.wires.push(Wire { a: [r2[0], da[1]], b: da });
        let r1 = p.parts[r].sch_xf([0.0, 5.08]);
        p.labels.push(Label { at: r1, text: "VCC".into(), kind: LabelKind::Power, rot: 0 });
        let dk = p.parts[d].sch_xf([0.0, -5.08]);
        p.labels.push(Label { at: dk, text: "GND".into(), kind: LabelKind::Ground, rot: 0 });
        let pins = p.sch_pins();
        let j1 = pins.iter().find(|x| x.part == j && x.num == "1").unwrap().at;
        let j2 = pins.iter().find(|x| x.part == j && x.num == "2").unwrap().at;
        p.labels.push(Label { at: j1, text: "VCC".into(), kind: LabelKind::Net, rot: 0 });
        p.labels.push(Label { at: j2, text: "GND".into(), kind: LabelKind::Net, rot: 0 });
        p.parts[r].at = [15.0, 20.0];
        p.parts[d].at = [25.0, 20.0];
        p.parts[j].at = [6.0, 20.0];
        p
    }

    #[test]
    fn netlist_follows_wires_and_labels() {
        let p = led_board();
        let nl = p.netlist();
        let names: Vec<&str> = nl.nets.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["GND", "Net-(D1-2)", "VCC"], "{names:?}");
        let vcc = &nl.nets[nl.by_name("VCC").unwrap()];
        assert!(vcc.power);
        assert_eq!(vcc.pins.len(), 2);
        let mid = &nl.nets[1];
        assert_eq!(mid.pins, vec![(1, "2".to_string()), (0, "2".to_string())]);
    }

    #[test]
    fn ratsnest_and_routing_close_the_nets() {
        let mut p = led_board();
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let air = ratsnest(&items(&p, &nl, &pads, &[]), &pads, &[]);
        assert_eq!(air.len(), 3);
        // route R2 to the LED anode on top
        let r2 = pads.iter().find(|x| x.part == 0 && x.num == "2").unwrap().at;
        let da = pads.iter().find(|x| x.part == 1 && x.num == "2").unwrap().at;
        p.board.tracks.push(Track { a: r2, b: da, width: 0.25, layer: Layer::Top, net: "Net-(D1-2)".into() });
        let air = ratsnest(&items(&p, &nl, &pads, &[]), &pads, &[]);
        assert_eq!(air.len(), 2);
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(issues.iter().filter(|i| i.message.starts_with("Unrouted")).count() == 2, "{issues:?}");
    }

    #[test]
    fn drc_finds_a_short() {
        let mut p = led_board();
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let a = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap().at;
        let b = pads.iter().find(|x| x.part == 1 && x.num == "1").unwrap().at;
        p.board.tracks.push(Track { a, b, width: 0.25, layer: Layer::Top, net: "VCC".into() });
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(issues.iter().any(|i| i.message.starts_with("Short")), "{issues:?}");
    }

    #[test]
    fn renamed_nets_keep_their_tracks() {
        let mut p = led_board();
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let r2 = pads.iter().find(|x| x.part == 0 && x.num == "2").unwrap().at;
        let da = pads.iter().find(|x| x.part == 1 && x.num == "2").unwrap().at;
        // round the LED's other pad
        let path = [r2, [r2[0], r2[1] + 2.0], [da[0], da[1] + 2.0], da];
        for s in path.windows(2) {
            p.board.tracks.push(Track { a: s[0], b: s[1], width: 0.25, layer: Layer::Top, net: "Net-(D1-2)".into() });
        }
        // renaming the LED changes the unnamed net's generated name
        p.parts[1].reference = "D7".into();
        let nl = p.netlist();
        assert!(nl.by_name("Net-(D1-2)").is_none());
        assert!(p.retag_copper(&nl));
        assert!(p.board.tracks.iter().all(|t| t.net == "Net-(D7-2)"));
        // and a relabelled supply carries its tracks and pours along
        let vcc_pad = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap().at;
        p.board.tracks.push(Track { a: vcc_pad, b: [vcc_pad[0], vcc_pad[1] - 3.0], width: 0.5, layer: Layer::Top, net: "VCC".into() });
        p.board.zones.push(Zone { net: "VCC".into(), layer: Layer::Bottom, outline: p.board.outline.clone(), ..Default::default() });
        for l in &mut p.labels {
            if l.text == "VCC" {
                l.text = "+5V".into();
            }
        }
        let nl = p.netlist();
        assert!(p.retag_copper(&nl));
        assert_eq!(p.board.tracks[3].net, "+5V");
        assert_eq!(p.board.zones[0].net, "+5V");
        let pads = p.pads(&nl);
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(!issues.iter().any(|i| i.message.starts_with("Short") && (i.message.contains("VCC") || i.message.contains("+5V"))), "{issues:?}");
    }

    #[test]
    fn labels_join_by_trimmed_text() {
        let mut p = led_board();
        for l in &mut p.labels {
            if l.text == "VCC" && l.kind == LabelKind::Net {
                l.text = "VCC ".into();
            }
        }
        let nl = p.netlist();
        assert_eq!(nl.nets.iter().filter(|n| n.name == "VCC").count(), 1);
        assert_eq!(nl.nets[nl.by_name("VCC").unwrap()].pins.len(), 2);
    }

    #[test]
    fn to92_pads_keep_the_clearance() {
        let mut p = Pcb::new("q");
        let q = p.add_part("Q_NPN", Some("TO-92"), None, [0.0, 0.0]).unwrap();
        for (k, sp) in p.sch_pins().iter().enumerate() {
            p.labels.push(Label { at: sp.at, text: format!("N{k}"), kind: LabelKind::Net, rot: 0 });
        }
        p.parts[q].at = [25.0, 20.0];
        let nl = p.netlist();
        assert_eq!(nl.nets.len(), 3);
        let pads = p.pads(&nl);
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn ground_pour_connects_ground_pads() {
        let mut p = led_board();
        p.board.zones.push(Zone { net: "GND".into(), layer: Layer::Top, outline: p.board.outline.clone(), ..Default::default() });
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let fills = fill_zones(&p, &nl, &pads);
        assert!(!fills[0].is_empty());
        let its = items(&p, &nl, &pads, &fills);
        let air = ratsnest(&its, &pads, &fills);
        assert!(!air.iter().any(|a| a.net == "GND"), "{air:?}");
        // the pour keeps clear of VCC pads
        let vcc = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap();
        assert!(fills[0].iter().all(|piece| !inside(&piece[0], vcc.at) || piece[1..].iter().any(|h| inside(h, vcc.at))));
    }

    fn in_fill(fill: &Fill, q: P) -> bool {
        fill.iter().any(|piece| inside(&piece[0], q) && !piece[1..].iter().any(|h| inside(h, q)))
    }

    #[test]
    fn thermal_reliefs_join_through_hole_pads() {
        let mut p = led_board();
        p.board.zones.push(Zone { net: "GND".into(), layer: Layer::Top, outline: p.board.outline.clone(), ..Default::default() });
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let j2 = pads.iter().find(|x| x.part == 2 && x.num == "2").unwrap().clone();
        let fills = fill_zones(&p, &nl, &pads);
        let r = j2.size[0].max(j2.size[1]) / 2.0;
        // the gap round the pad is clear on the diagonal, a spoke crosses it straight out
        let diag = add(j2.at, [(r + 0.15) * 0.7072, (r + 0.15) * 0.7072]);
        assert!(!in_fill(&fills[0], diag), "relief gap is filled");
        assert!(in_fill(&fills[0], add(j2.at, [j2.size[0] / 2.0 + 0.15, 0.0])), "no spoke");
        let its = items(&p, &nl, &pads, &fills);
        assert!(!ratsnest(&its, &pads, &fills).iter().any(|a| a.net == "GND"));
        let issues = drc(&p, &nl, &pads, &fills);
        assert!(!issues.iter().any(|i| i.rule == "thermal"), "{issues:?}");
        // a solid pour covers the gap
        p.board.zones[0].pads = PadConnect::Solid;
        let fills = fill_zones(&p, &nl, &pads);
        assert!(in_fill(&fills[0], diag));
    }

    #[test]
    fn net_classes_set_clearance_width_and_vias() {
        let mut p = led_board();
        p.board.rules.classes.push(NetClass { name: "HV".into(), nets: vec!["VCC".into()], clearance: 1.0, track: 0.8, via_d: 0.9, via_drill: 0.4, ..Default::default() });
        let nl = p.netlist();
        let r = p.net_rules(&nl, "VCC");
        assert_eq!((r.clearance, r.track, r.via_d, r.via_drill), (1.0, 0.8, 0.9, 0.4));
        assert_eq!(p.track_width(&nl, "GND"), p.board.rules.power_track);
        assert_eq!(p.track_width(&nl, "Net-(D1-2)"), p.board.rules.track);
        // a GND track 0.5 mm from a VCC pad breaks the class clearance, not the board's
        let pads = p.pads(&nl);
        let vcc = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap();
        let (lo, hi) = vcc.cu().bbox();
        let y = hi[1] + 0.5 + 0.125;
        p.board.tracks.push(Track { a: [lo[0] - 2.0, y], b: [hi[0] + 2.0, y], width: 0.25, layer: Layer::Top, net: "GND".into() });
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(issues.iter().any(|i| i.rule == "clearance" && i.message.contains("< 1 mm")), "{issues:?}");
        // and the autorouter gives VCC its width and vias
        p.board.tracks.clear();
        crate::pcb_route::autoroute(&mut p, None);
        assert!(p.board.tracks.iter().any(|t| t.net == "VCC"));
        assert!(p.board.tracks.iter().filter(|t| t.net == "VCC").all(|t| (t.width - 0.8).abs() < 1e-9 || (t.width - p.board.rules.track).abs() < 1e-9));
        assert!(p.board.vias.iter().filter(|v| v.net == "VCC").all(|v| v.d == 0.9));
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(!issues.iter().any(|i| i.rule == "clearance" || i.rule == "short"), "{issues:?}");
    }

    #[test]
    fn keepouts_hold_tracks_and_pours_out() {
        let mut p = led_board();
        let ko = vec![[30.0, 5.0], [40.0, 5.0], [40.0, 15.0], [30.0, 15.0]];
        p.board.keepouts.push(Keepout { outline: ko, ..Default::default() });
        p.board.zones.push(Zone { net: "GND".into(), layer: Layer::Bottom, outline: p.board.outline.clone(), ..Default::default() });
        p.board.tracks.push(Track { a: [28.0, 10.0], b: [42.0, 10.0], width: 0.25, layer: Layer::Top, net: "GND".into() });
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let fills = fill_zones(&p, &nl, &pads);
        assert!(!in_fill(&fills[0], [35.0, 10.0]));
        assert!(in_fill(&fills[0], [35.0, 30.0]));
        let issues = drc(&p, &nl, &pads, &fills);
        assert!(issues.iter().any(|i| i.rule == "keepout"), "{issues:?}");
        // a keepout on the bottom only lets top tracks through
        p.board.keepouts[0].layer = Some(Layer::Bottom);
        let issues = drc(&p, &nl, &pads, &fills);
        assert!(!issues.iter().any(|i| i.rule == "keepout"), "{issues:?}");
    }

    #[test]
    fn holes_bodies_heights_and_antennas() {
        let mut p = led_board();
        p.board.holes.push(Hole { at: [40.0, 30.0], d: 3.2 });
        p.board.holes.push(Hole { at: [43.3, 30.0], d: 3.2 });
        // the LED on top of the resistor
        p.parts[1].at = [15.3, 20.0];
        // a track from the resistor's VCC pad to nowhere
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let r1 = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap().at;
        p.board.tracks.push(Track { a: r1, b: [r1[0], r1[1] + 6.0], width: 0.25, layer: Layer::Top, net: "VCC".into() });
        p.board.rules.max_height = 2.0;
        let issues = drc(&p, &nl, &pads, &[]);
        let has = |rule: &str| issues.iter().any(|i| i.rule == rule);
        assert!(has("hole_gap"), "{issues:?}");
        assert!(issues.iter().any(|i| i.rule == "body" && i.message.contains("R1") && i.message.contains("D1")), "{issues:?}");
        assert!(issues.iter().any(|i| i.rule == "height" && i.message.starts_with("J1")), "{issues:?}");
        assert_eq!(issues.iter().filter(|i| i.rule == "antenna").count(), 1, "{issues:?}");
        // checks can be turned off
        p.board.rules.off = vec!["antenna".into(), "unrouted".into()];
        let issues = drc(&p, &nl, &pads, &[]);
        assert!(!issues.iter().any(|i| i.rule == "antenna" || i.rule == "unrouted"));
        // apart again, the bodies are clear
        p.parts[1].at = [25.0, 20.0];
        let pads = p.pads(&nl);
        assert!(!drc(&p, &nl, &pads, &[]).iter().any(|i| i.rule == "body"));
    }
}
