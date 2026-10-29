//! CAM: Manufacture tabs with a setup (part, stock, work origin), a tool
//! library and 2.5D milling operations. Toolpaths are made in `cam_paths`
//! and turned into G-code in `cam_post`.

use serde::{Deserialize, Serialize};

use crate::doc::BodyId;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Flat (square) end mill.
    Flat,
    Ball,
    /// Bull nose: flat end mill with a corner radius.
    Bull,
    /// Tapered ball end mill: ball tip, conical sides.
    TaperedBall,
    /// Chamfer mill: conical cutter with a flat tip.
    Chamfer,
    /// V-bit engraver.
    Engrave,
    /// Slot mill (T-slot / keyseat / woodruff cutter): cuts sideways below a thin neck.
    Slot,
    /// Single-profile or multi-tooth thread mill.
    ThreadMill,
    /// Face mill (indexable, large diameter).
    FaceMill,
    Drill,
    SpotDrill,
    CenterDrill,
    Tap,
    Reamer,
}

impl ToolKind {
    pub const ALL: [ToolKind; 14] = [
        ToolKind::Flat,
        ToolKind::Ball,
        ToolKind::Bull,
        ToolKind::TaperedBall,
        ToolKind::Chamfer,
        ToolKind::Engrave,
        ToolKind::Slot,
        ToolKind::ThreadMill,
        ToolKind::FaceMill,
        ToolKind::Drill,
        ToolKind::SpotDrill,
        ToolKind::CenterDrill,
        ToolKind::Tap,
        ToolKind::Reamer,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Flat => "Flat end mill",
            ToolKind::Ball => "Ball end mill",
            ToolKind::Bull => "Bull nose end mill",
            ToolKind::TaperedBall => "Tapered ball mill",
            ToolKind::Chamfer => "Chamfer mill",
            ToolKind::Engrave => "Engraving V-bit",
            ToolKind::Slot => "Slot mill",
            ToolKind::ThreadMill => "Thread mill",
            ToolKind::FaceMill => "Face mill",
            ToolKind::Drill => "Drill",
            ToolKind::SpotDrill => "Spot drill",
            ToolKind::CenterDrill => "Center drill",
            ToolKind::Tap => "Tap",
            ToolKind::Reamer => "Reamer",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            ToolKind::Flat => "flat",
            ToolKind::Ball => "ball",
            ToolKind::Bull => "bull",
            ToolKind::TaperedBall => "tapered_ball",
            ToolKind::Chamfer => "chamfer",
            ToolKind::Engrave => "engrave",
            ToolKind::Slot => "slot",
            ToolKind::ThreadMill => "thread_mill",
            ToolKind::FaceMill => "face_mill",
            ToolKind::Drill => "drill",
            ToolKind::SpotDrill => "spot_drill",
            ToolKind::CenterDrill => "center_drill",
            ToolKind::Tap => "tap",
            ToolKind::Reamer => "reamer",
        }
    }

    pub fn parse(s: &str) -> Option<ToolKind> {
        let s = s.trim().to_lowercase().replace([' ', '-'], "_");
        ToolKind::ALL.into_iter().find(|k| k.key() == s || k.label().to_lowercase().replace(' ', "_") == s).or(match s.as_str() {
            "endmill" | "end_mill" | "flat_endmill" | "square" => Some(ToolKind::Flat),
            "ballmill" | "ball_mill" | "ball_endmill" => Some(ToolKind::Ball),
            "bullnose" | "bull_nose" | "corner_radius" => Some(ToolKind::Bull),
            "tapered" | "taper" | "tapered_ball_mill" => Some(ToolKind::TaperedBall),
            "vbit" | "v_bit" => Some(ToolKind::Engrave),
            "tslot" | "t_slot" | "keyseat" | "woodruff" | "slot_mill" => Some(ToolKind::Slot),
            "threadmill" => Some(ToolKind::ThreadMill),
            "facemill" => Some(ToolKind::FaceMill),
            "spot" => Some(ToolKind::SpotDrill),
            "center" => Some(ToolKind::CenterDrill),
            _ => None,
        })
    }

    /// Only plunges along its axis.
    pub fn is_hole_tool(self) -> bool {
        matches!(self, ToolKind::Drill | ToolKind::SpotDrill | ToolKind::CenterDrill | ToolKind::Tap | ToolKind::Reamer)
    }
}

/// Coolant: off, flood (M8) or mist (M7). `Auto` on a tool means "as the setup says"; on a setup it means
/// flood for metal stock and off for wood, plastic and foam.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum Coolant {
    #[default]
    Auto,
    Off,
    Flood,
    Mist,
}

impl Coolant {
    pub const ALL: [Coolant; 4] = [Coolant::Auto, Coolant::Off, Coolant::Flood, Coolant::Mist];

    pub fn label(self) -> &'static str {
        match self {
            Coolant::Auto => "auto",
            Coolant::Off => "off",
            Coolant::Flood => "flood (M8)",
            Coolant::Mist => "mist (M7)",
        }
    }

    pub fn is_on(self) -> bool {
        matches!(self, Coolant::Flood | Coolant::Mist)
    }
}

/// Coolant as a name, or as the true/false of older files (true = flood, false = the setup's).
fn coolant_or_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Coolant, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum C {
        B(bool),
        C(Coolant),
    }
    Ok(match C::deserialize(d)? {
        C::B(true) => Coolant::Flood,
        C::B(false) => Coolant::Auto,
        C::C(c) => c,
    })
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_default_ratio(r: &f64) -> bool {
    *r == 1.75
}

/// A cutting tool. Lengths are mm, feeds mm/min.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tool {
    pub id: u64,
    /// Tool number in the machine (T1, T2, …).
    pub number: u32,
    pub name: String,
    pub kind: ToolKind,
    /// Cutting diameter (for chamfer mills and V-bits the largest diameter;
    /// for thread mills the diameter over the teeth).
    pub diameter: f64,
    /// Corner radius of a bull nose; tip radius of a tapered ball mill.
    pub corner_radius: f64,
    /// Flat tip diameter of a chamfer mill or V-bit.
    pub tip_diameter: f64,
    /// Included angle in degrees: chamfer mill and V-bit (90 = 45° chamfer), drill point (118/135),
    /// spot drill (90/120); for a tapered ball mill the angle per side.
    pub angle: f64,
    pub flute_length: f64,
    /// Length out of the holder.
    pub stickout: f64,
    /// Diameter of the tool holder (collet nut or holder body), for collision checks.
    pub holder_diameter: f64,
    /// Holder outline from its bottom face up, as (diameter, height) points; empty = a plain
    /// cylinder of `holder_diameter`. Above the last point it carries on at that diameter (the spindle).
    pub holder: Vec<[f64; 2]>,
    /// Name of the holder preset the outline came from.
    pub holder_name: String,
    pub shank_diameter: f64,
    pub flutes: u32,
    /// Thread pitch of a thread mill or tap.
    pub pitch: f64,
    /// Cutting height of a slot mill (its thickness) or a thread mill's toothed length.
    pub cutting_width: f64,
    /// Neck diameter of a slot mill or thread mill.
    pub neck_diameter: f64,
    /// "carbide", "hss" or "cobalt"
    pub material: String,
    /// Coolant for this tool; `Auto` = the setup's.
    #[serde(deserialize_with = "coolant_or_bool")]
    pub coolant: Coolant,
    /// Taps: held in a self-reversing tapping head, so the spindle keeps turning forward and the program
    /// just feeds in and out (no G84, no spindle reversal). Needed on a high-speed spindle.
    #[serde(skip_serializing_if = "is_false")]
    pub tapping_head: bool,
    /// The tapping head's reverse ratio: it unscrews the tap this many times faster than it went in,
    /// so the program pulls out that much faster.
    #[serde(skip_serializing_if = "is_default_ratio")]
    pub reverse_ratio: f64,
    pub rpm: f64,
    pub feed: f64,
    pub plunge_feed: f64,
    /// Default depth per pass.
    pub stepdown: f64,
    /// Default stepover as a fraction of the diameter.
    pub stepover: f64,
}

impl Default for Tool {
    fn default() -> Self {
        Tool {
            id: 0,
            number: 1,
            name: String::new(),
            kind: ToolKind::Flat,
            diameter: 6.0,
            corner_radius: 0.0,
            tip_diameter: 0.0,
            angle: 0.0,
            flute_length: 20.0,
            stickout: 30.0,
            holder_diameter: 30.0,
            holder: vec![],
            holder_name: String::new(),
            shank_diameter: 6.0,
            flutes: 2,
            pitch: 0.0,
            cutting_width: 0.0,
            neck_diameter: 0.0,
            material: "carbide".into(),
            coolant: Coolant::Auto,
            tapping_head: false,
            reverse_ratio: 1.75,
            rpm: 12000.0,
            feed: 1000.0,
            plunge_feed: 300.0,
            stepdown: 2.0,
            stepover: 0.4,
        }
    }
}

impl Tool {
    pub fn radius(&self) -> f64 {
        self.diameter / 2.0
    }

    /// Short description: "Ø6 flat end mill, 2 flutes".
    pub fn describe(&self) -> String {
        let d = fmt_mm(self.diameter);
        match self.kind {
            ToolKind::Bull => format!("Ø{d} R{} bull nose, {} flutes", fmt_mm(self.corner_radius), self.flutes),
            ToolKind::TaperedBall => format!("R{} tapered ball, {}° per side, Ø{d} shank", fmt_mm(self.corner_radius), fmt_mm(self.angle)),
            ToolKind::Chamfer | ToolKind::Engrave => format!("Ø{d} {}° {}", fmt_mm(self.angle), self.kind.label().to_lowercase()),
            ToolKind::Slot => format!("Ø{d} × {} slot mill", fmt_mm(self.cutting_width)),
            ToolKind::ThreadMill => format!("Ø{d} thread mill, pitch {}", fmt_mm(self.pitch)),
            ToolKind::Tap => format!("M{} × {} tap", fmt_mm(self.diameter), fmt_mm(self.pitch)),
            ToolKind::Drill | ToolKind::SpotDrill | ToolKind::CenterDrill => format!("Ø{d} {}° {}", fmt_mm(self.angle), self.kind.label().to_lowercase()),
            _ => format!("Ø{d} {}, {} flutes", self.kind.label().to_lowercase(), self.flutes),
        }
    }

    /// Holder outline as (radius, height above the holder's bottom face) points going up.
    pub fn holder_outline(&self) -> Vec<(f64, f64)> {
        if self.holder.len() >= 2 {
            self.holder.iter().map(|p| (p[0].max(0.0) / 2.0, p[1])).collect()
        } else {
            let r = (self.holder_diameter / 2.0).max(self.shank_diameter / 2.0);
            vec![(r, 0.0), (r, 1.0)]
        }
    }

    /// Height of the conical tip of a drill or spot drill.
    pub fn tip_length(&self) -> f64 {
        if matches!(self.kind, ToolKind::Drill | ToolKind::SpotDrill | ToolKind::CenterDrill) && self.angle > 1.0 {
            self.radius() / (self.angle.to_radians() / 2.0).tan()
        } else {
            0.0
        }
    }

    /// Profile of the cutter as (radius, height above the tip) points from the tip up, for drawing and simulation.
    pub fn profile(&self) -> Vec<(f64, f64)> {
        let r = self.radius();
        let len = self.flute_length.max(r);
        match self.kind {
            ToolKind::Ball => {
                let mut v: Vec<(f64, f64)> = (0..=8).map(|i| i as f64 / 8.0 * std::f64::consts::FRAC_PI_2).map(|a| (r * a.sin(), r - r * a.cos())).collect();
                v.push((r, len));
                v
            }
            ToolKind::Bull => {
                let cr = self.corner_radius.clamp(0.0, r);
                let mut v = vec![(0.0, 0.0)];
                v.extend((0..=6).map(|i| i as f64 / 6.0 * std::f64::consts::FRAC_PI_2).map(|a| (r - cr + cr * a.sin(), cr - cr * a.cos())));
                v.push((r, len));
                v
            }
            ToolKind::TaperedBall => {
                let tr = self.corner_radius.max(0.01);
                let half = self.angle.to_radians().clamp(0.0, 1.4);
                let mut v: Vec<(f64, f64)> = (0..=8).map(|i| i as f64 / 8.0 * (std::f64::consts::FRAC_PI_2 - half)).map(|a| (tr * a.sin(), tr - tr * a.cos())).collect();
                let (r0, z0) = *v.last().unwrap();
                let z1 = z0 + ((r - r0).max(0.0) / half.tan().max(1e-6)).min(len);
                v.push((r, z1.max(z0)));
                v.push((r, len.max(z1)));
                v
            }
            ToolKind::Chamfer | ToolKind::Engrave | ToolKind::SpotDrill | ToolKind::CenterDrill | ToolKind::Drill => {
                let tip = if matches!(self.kind, ToolKind::Chamfer) { self.tip_diameter / 2.0 } else { 0.0 };
                let half = (self.angle.max(1.0) / 2.0).to_radians();
                let h = (r - tip).max(0.0) / half.tan();
                vec![(0.0, 0.0), (tip, 0.0), (r, h), (r, len.max(h))]
            }
            ToolKind::Slot | ToolKind::ThreadMill => {
                let neck = if self.neck_diameter > 0.0 { self.neck_diameter / 2.0 } else { r * 0.6 };
                let w = self.cutting_width.max(0.5);
                vec![(0.0, 0.0), (r, 0.0), (r, w), (neck, w), (neck, len.max(w * 2.0))]
            }
            _ => vec![(0.0, 0.0), (r, 0.0), (r, len)],
        }
    }
}

/// How far above the holder's bottom face its underside is at `rho` from the axis (None = outside it).
/// The outline carries on straight up from its last point.
pub fn holder_underside(outline: &[(f64, f64)], rho: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    let mut keep = |z: f64| best = Some(best.map(|b: f64| b.min(z)).unwrap_or(z));
    for w in outline.windows(2) {
        let ((r0, z0), (r1, z1)) = (w[0], w[1]);
        if r0 >= rho {
            keep(z0);
        } else if r1 >= rho {
            keep(z0 + (z1 - z0) * (rho - r0) / (r1 - r0));
        }
    }
    if let Some(&(r, z)) = outline.last() {
        if r >= rho {
            keep(z);
        }
    }
    best
}

/// Tool holder presets: name and outline as (diameter, height) from the bottom face up.
pub fn holder_presets() -> Vec<(&'static str, Vec<[f64; 2]>)> {
    // collet chuck nuts (DIN 6499) on a holder body, then the spindle nose
    let er = |nut: f64, len: f64, body: f64, spindle: f64| {
        vec![[nut - 4.0, 0.0], [nut, 3.0], [nut, len], [body, len], [body, len + 40.0], [spindle, len + 40.0], [spindle, len + 120.0]]
    };
    vec![
        ("ER11 collet, router spindle Ø52", er(19.0, 11.5, 19.0, 52.0)),
        ("ER11 collet chuck", er(19.0, 11.5, 25.0, 60.0)),
        ("ER16 collet chuck", er(28.0, 17.5, 32.0, 63.0)),
        ("ER20 collet, router spindle Ø80", er(34.0, 19.5, 34.0, 80.0)),
        ("ER20 collet chuck", er(34.0, 19.5, 40.0, 63.0)),
        ("ER25 collet chuck", er(42.0, 20.0, 46.0, 63.0)),
        ("ER32 collet chuck", er(50.0, 22.5, 53.0, 100.0)),
        ("Shrink-fit holder Ø6", vec![[14.0, 0.0], [24.0, 50.0], [44.0, 50.0], [44.0, 90.0], [63.0, 90.0], [63.0, 160.0]]),
        ("Shrink-fit holder Ø12", vec![[22.0, 0.0], [32.0, 50.0], [44.0, 50.0], [44.0, 90.0], [63.0, 90.0], [63.0, 160.0]]),
        ("Side-lock (Weldon) holder", vec![[40.0, 0.0], [40.0, 50.0], [63.0, 50.0], [63.0, 120.0]]),
    ]
}

/// Put a holder preset on a tool by name.
pub fn set_holder(t: &mut Tool, name: &str) -> bool {
    let Some((n, h)) = holder_presets().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())) else { return false };
    t.holder_diameter = h[1][0];
    t.holder = h;
    t.holder_name = n.into();
    true
}

pub fn fmt_mm(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

// ---------------------------------------------------------------- feeds and speeds

/// Stock materials with a cutting speed (m/min, carbide) and a chip load factor.
pub const MATERIALS: [(&str, f64, f64); 9] = [
    ("Aluminium", 300.0, 1.0),
    ("Brass", 200.0, 0.9),
    ("Copper", 150.0, 0.7),
    ("Mild steel", 120.0, 0.6),
    ("Stainless steel", 80.0, 0.45),
    ("Plastic", 250.0, 1.3),
    ("Hardwood", 400.0, 1.6),
    ("Softwood / MDF", 500.0, 2.0),
    ("Foam", 600.0, 3.0),
];

fn material(name: &str) -> (f64, f64) {
    MATERIALS.iter().find(|m| m.0.eq_ignore_ascii_case(name)).map(|m| (m.1, m.2)).unwrap_or((300.0, 1.0))
}

/// Suggested spindle speed, feed, plunge feed, stepdown and stepover for a tool in a stock material.
/// `max_rpm` is the spindle's limit, `min_rpm` the slowest it keeps its torque at (0: no limit).
pub fn suggest(tool: &mut Tool, stock: &str, max_rpm: f64, min_rpm: f64) {
    let (vc, k) = material(stock);
    let d = tool.diameter.max(0.1);
    let hss = tool.material != "carbide";
    let vc = if hss { vc * 0.35 } else { vc };
    let rpm = (vc * 1000.0 / (std::f64::consts::PI * d)).min(max_rpm.max(1000.0)).max(1000.0).max(min_rpm.min(max_rpm));
    // chip load per tooth grows roughly with diameter
    let fz = (0.008 * d.powf(0.8)).clamp(0.005, 0.12) * k;
    let flutes = tool.flutes.max(1) as f64;
    tool.rpm = (rpm / 100.0).round() * 100.0;
    match tool.kind {
        ToolKind::Tap => {
            tool.rpm = (tool.rpm * 0.08).clamp(100.0, 800.0).round();
            tool.feed = tool.rpm * tool.pitch;
            tool.plunge_feed = tool.feed;
        }
        ToolKind::Drill | ToolKind::Reamer | ToolKind::SpotDrill | ToolKind::CenterDrill => {
            tool.rpm = ((tool.rpm * 0.5 / 100.0).round().max(5.0) * 100.0).max(min_rpm.min(max_rpm));
            let f = tool.rpm * (0.02 * d).clamp(0.02, 0.3) * k.min(1.5);
            tool.feed = f.round();
            tool.plunge_feed = tool.feed;
        }
        ToolKind::ThreadMill => {
            tool.feed = (tool.rpm * fz * 0.5 * flutes).round();
            tool.plunge_feed = (tool.feed * 0.5).round();
        }
        _ => {
            tool.feed = (tool.rpm * fz * flutes).round();
            tool.plunge_feed = (tool.feed * 0.3).round();
        }
    }
    tool.stepdown = match tool.kind {
        ToolKind::FaceMill => (d * 0.05).clamp(0.5, 2.0),
        ToolKind::Ball | ToolKind::TaperedBall => (d * 0.25).max(0.2),
        _ => (d * 0.5 * k.min(1.5)).clamp(0.2, d * 1.5),
    };
    tool.stepdown = (tool.stepdown * 10.0).round() / 10.0;
    tool.stepover = match tool.kind {
        ToolKind::FaceMill => 0.7,
        ToolKind::Ball | ToolKind::TaperedBall => 0.15,
        _ => 0.4,
    };
}

// ---------------------------------------------------------------- library

/// ISO metric coarse threads: (major diameter, pitch).
pub const METRIC_COARSE: [(f64, f64); 13] =
    [(2.0, 0.4), (2.5, 0.45), (3.0, 0.5), (4.0, 0.7), (5.0, 0.8), (6.0, 1.0), (8.0, 1.25), (10.0, 1.5), (12.0, 1.75), (14.0, 2.0), (16.0, 2.0), (20.0, 2.5), (24.0, 3.0)];

/// The built-in tool library: the most common sizes of each kind.
pub fn presets() -> Vec<Tool> {
    let mut v = vec![];
    let mut add = |kind: ToolKind, d: f64, f: &dyn Fn(&mut Tool)| {
        let mut t =
            Tool { kind, diameter: d, shank_diameter: d.max(3.0).min(12.0), flute_length: (d * 3.0).clamp(3.0, 40.0), stickout: (d * 4.0).clamp(15.0, 60.0), ..Default::default() };
        f(&mut t);
        if t.name.is_empty() {
            t.name = t.describe();
        }
        v.push(t);
    };
    for d in [1.0, 2.0, 3.0, 3.175, 4.0, 6.0, 6.35, 8.0, 10.0, 12.0] {
        add(ToolKind::Flat, d, &|t| t.flutes = if t.diameter < 3.0 { 2 } else { 3 });
    }
    for d in [1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 10.0] {
        add(ToolKind::Ball, d, &|t| {
            t.flutes = 2;
            t.corner_radius = t.diameter / 2.0;
        });
    }
    for (d, r) in [(6.0, 0.5), (8.0, 1.0), (10.0, 1.0), (12.0, 2.0)] {
        add(ToolKind::Bull, d, &|t| {
            t.corner_radius = r;
            t.flutes = 4;
        });
    }
    for (tip_r, per_side, shank) in [(0.25, 1.0, 3.175), (0.5, 1.5, 3.175), (0.5, 3.6, 6.0), (1.0, 5.0, 6.0)] {
        add(ToolKind::TaperedBall, shank, &|t| {
            t.corner_radius = tip_r;
            t.angle = per_side;
            t.flutes = 2;
            t.flute_length = 25.0;
        });
    }
    for (d, a, tip) in [(6.0, 90.0, 0.5), (10.0, 90.0, 1.0), (12.0, 60.0, 1.0), (12.0, 120.0, 1.0)] {
        add(ToolKind::Chamfer, d, &|t| {
            t.angle = a;
            t.tip_diameter = tip;
            t.flutes = 4;
        });
    }
    for (d, a) in [(3.175, 30.0), (6.35, 60.0), (6.35, 90.0)] {
        add(ToolKind::Engrave, d, &|t| {
            t.angle = a;
            t.flutes = 1;
        });
    }
    for (d, w, neck) in [(12.0, 1.0, 6.0), (16.0, 3.0, 8.0), (20.0, 5.0, 10.0), (25.0, 6.0, 12.0)] {
        add(ToolKind::Slot, d, &|t| {
            t.cutting_width = w;
            t.neck_diameter = neck;
            t.flutes = 6;
            t.shank_diameter = neck.max(6.0);
        });
    }
    for (major, pitch) in [(3.0_f64, 0.5_f64), (4.0, 0.7), (5.0, 0.8), (6.0, 1.0), (8.0, 1.25), (10.0, 1.5), (12.0, 1.75)] {
        // a thread mill must fit in the tap drill hole
        let d = ((major - pitch) * 0.75 * 10.0).floor() / 10.0;
        add(ToolKind::ThreadMill, d, &|t| {
            t.pitch = pitch;
            t.cutting_width = pitch * 2.0;
            t.neck_diameter = d * 0.7;
            t.flutes = 3;
            t.name = format!("M{} × {} thread mill (Ø{})", fmt_mm(major), fmt_mm(pitch), fmt_mm(d));
        });
    }
    for d in [40.0, 50.0, 63.0] {
        add(ToolKind::FaceMill, d, &|t| {
            t.flutes = 4;
            t.shank_diameter = 22.0;
            t.flute_length = 6.0;
        });
    }
    // jobber drills: the usual sizes and the metric tap drills
    for d in [1.0, 1.5, 2.0, 2.5, 3.0, 3.3, 3.5, 4.0, 4.2, 5.0, 5.5, 6.0, 6.8, 8.0, 8.5, 10.0, 10.2, 12.0] {
        add(ToolKind::Drill, d, &|t| {
            t.angle = 118.0;
            t.flutes = 2;
            t.flute_length = (t.diameter * 8.0).clamp(10.0, 100.0);
            t.stickout = t.flute_length + 10.0;
            t.material = "hss".into();
        });
    }
    for d in [6.0, 10.0] {
        add(ToolKind::SpotDrill, d, &|t| {
            t.angle = 90.0;
            t.flutes = 2;
        });
    }
    add(ToolKind::CenterDrill, 3.15, &|t| {
        t.angle = 60.0;
        t.material = "hss".into();
    });
    for (major, pitch) in METRIC_COARSE.into_iter().filter(|m| m.0 >= 3.0 && m.0 <= 12.0) {
        add(ToolKind::Tap, major, &|t| {
            t.pitch = pitch;
            t.flutes = 3;
            t.material = "hss".into();
            t.flute_length = major * 2.0;
        });
    }
    for d in [3.0, 4.0, 5.0, 6.0, 8.0, 10.0] {
        add(ToolKind::Reamer, d, &|t| {
            t.flutes = 6;
            t.material = "carbide".into();
            t.name = format!("Ø{} H7 reamer", fmt_mm(t.diameter));
        });
    }
    for t in v.iter_mut() {
        suggest(t, "Aluminium", 24000.0, 0.0);
    }
    v
}

pub(crate) fn library_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(std::path::PathBuf::from))?;
    Some(base.join("cad-rust-app").join("tools.json"))
}

/// The user's own tools, kept in ~/.config/cad-rust-app/tools.json.
pub fn load_library() -> Vec<Tool> {
    library_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_library(tools: &[Tool]) {
    if let Some(p) = library_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(tools).unwrap_or_default());
    }
}

// ---------------------------------------------------------------- operations

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// Flatten the top of the stock down to the part's top.
    Face,
    /// Clear all the stock around the part, level by level (roughing).
    Clear,
    /// Clear closed pockets.
    Pocket,
    /// Profile the outline (outside or inside walls).
    Contour,
    /// Chamfer or deburr edges with a chamfer mill, V-bit or spot drill.
    Chamfer,
    /// Drill, spot or ream holes, with optional pecking.
    Drill,
    /// Helical boring of holes larger than the end mill.
    Bore,
    /// Internal threads with a thread mill.
    Thread,
    /// Tapping (rigid or floating).
    Tap,
    /// 3D roughing with cycloidal (trochoidal) loops that keep the tool's engagement constant.
    Adaptive,
    /// 3D finishing along parallel lines.
    Parallel,
    /// 3D finishing at constant heights (steep walls).
    Waterline,
    /// 3D finishing with a constant stepover measured over the surface.
    Scallop,
    /// Chamfer or break every sharp edge in 3D (or the picked ones).
    Deburr,
    /// 3D clean-up of the corners and fillets a bigger tool couldn't reach.
    Pencil,
    /// Follow sketch curves at a depth below the surface (engraving lines, cutting along a drawing).
    Trace,
    /// Carve the closed shapes of a sketch (text, logos) with a V-bit: deeper where they are wider,
    /// so the walls meet in sharp corners.
    VCarve,
}

impl OpKind {
    pub const ALL: [OpKind; 17] = [
        OpKind::Face,
        OpKind::Clear,
        OpKind::Pocket,
        OpKind::Contour,
        OpKind::Chamfer,
        OpKind::Drill,
        OpKind::Bore,
        OpKind::Thread,
        OpKind::Tap,
        OpKind::Adaptive,
        OpKind::Parallel,
        OpKind::Waterline,
        OpKind::Scallop,
        OpKind::Deburr,
        OpKind::Pencil,
        OpKind::Trace,
        OpKind::VCarve,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OpKind::Face => "Face",
            OpKind::Clear => "Clearing",
            OpKind::Pocket => "Pocket",
            OpKind::Contour => "Contour",
            OpKind::Chamfer => "Chamfer",
            OpKind::Drill => "Drill",
            OpKind::Bore => "Bore",
            OpKind::Thread => "Thread",
            OpKind::Tap => "Tap",
            OpKind::Adaptive => "Adaptive",
            OpKind::Parallel => "Parallel",
            OpKind::Waterline => "Waterline",
            OpKind::Scallop => "Scallop",
            OpKind::Deburr => "Deburr",
            OpKind::Pencil => "Pencil",
            OpKind::Trace => "Trace",
            OpKind::VCarve => "V-carve",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            OpKind::Face => "face",
            OpKind::Clear => "clear",
            OpKind::Pocket => "pocket",
            OpKind::Contour => "contour",
            OpKind::Chamfer => "chamfer",
            OpKind::Drill => "drill",
            OpKind::Bore => "bore",
            OpKind::Thread => "thread",
            OpKind::Tap => "tap",
            OpKind::Adaptive => "adaptive",
            OpKind::Parallel => "parallel",
            OpKind::Waterline => "waterline",
            OpKind::Scallop => "scallop",
            OpKind::Deburr => "deburr",
            OpKind::Pencil => "pencil",
            OpKind::Trace => "trace",
            OpKind::VCarve => "vcarve",
        }
    }

    pub fn parse(s: &str) -> Option<OpKind> {
        let s = s.trim().to_lowercase();
        OpKind::ALL.into_iter().find(|k| k.key() == s || k.label().to_lowercase() == s).or(match s.as_str() {
            "facing" => Some(OpKind::Face),
            "rough" | "roughing" | "clearing" => Some(OpKind::Clear),
            "adaptive_clear" | "trochoidal" | "cycloidal" | "hsm" => Some(OpKind::Adaptive),
            "raster" | "parallel_finish" => Some(OpKind::Parallel),
            "z_level" | "zlevel" | "contour_3d" => Some(OpKind::Waterline),
            "constant_stepover" => Some(OpKind::Scallop),
            "chamfer_3d" | "3d_chamfer" | "edge_break" => Some(OpKind::Deburr),
            "corner" | "corners" | "rest_finish" | "rest_finishing" | "cleanup" | "corner_cleanup" => Some(OpKind::Pencil),
            "profile" | "outline" => Some(OpKind::Contour),
            "spot" | "ream" | "peck" => Some(OpKind::Drill),
            "helix" | "helical" | "circular" => Some(OpKind::Bore),
            "thread_mill" | "threadmill" => Some(OpKind::Thread),
            "tapping" => Some(OpKind::Tap),
            "engrave" | "engraving" | "follow" | "sketch" | "2d_contour" => Some(OpKind::Trace),
            "v_carve" | "v-carve" | "vcarving" | "v_carving" | "carve" => Some(OpKind::VCarve),
            _ => None,
        })
    }

    pub fn description(self) -> &'static str {
        match self {
            OpKind::Face => "Flattens the top of the stock down to the top of the part.",
            OpKind::Clear => "Removes all the stock around the part, level by level, leaving the stock to leave for finishing.",
            OpKind::Pocket => "Clears closed pockets down to their floors, spiralling out from the middle.",
            OpKind::Contour => "Follows the part's outline (outside walls, inside walls or both), with optional holding tabs.",
            OpKind::Chamfer => "Runs a chamfer mill or V-bit along the top edges to break or chamfer them.",
            OpKind::Drill => "Drills the holes it finds (or the points given), with pecking for deep holes.",
            OpKind::Bore => "Helically interpolates holes bigger than the end mill.",
            OpKind::Thread => "Mills internal threads in holes with a thread mill.",
            OpKind::Tap => "Taps holes; rigid tapping on LinuxCNC/Fanuc, floating on GRBL.",
            OpKind::Adaptive => "3D roughing level by level with cycloidal loops: the tool takes a constant small stepover at full depth, so it can cut deep and fast.",
            OpKind::Parallel => "3D finishing along parallel lines at an angle, optionally only on shallow or steep areas.",
            OpKind::Waterline => "3D finishing at constant heights; best on steep walls.",
            OpKind::Scallop => "3D finishing with a constant stepover measured over the surface, working in from the outline.",
            OpKind::Deburr => "Chamfers or breaks every sharp convex edge in 3D with a chamfer mill or V-bit (or only the picked edges).",
            OpKind::Pencil => "3D clean-up with a small tool along the inside corners and fillets that a bigger (reference) tool left material in.",
            OpKind::Trace => "Follows the curves of sketches at a depth below the surface: engraved lines, or cutting along a drawing (on the line, or beside closed shapes).",
            OpKind::VCarve => "Carves the closed shapes of sketches (text, logos) with a V-bit, deeper where they are wider so the walls meet in sharp corners.",
        }
    }

    /// Whether it can machine only what earlier operations left (rest machining).
    pub fn uses_stock(self) -> bool {
        matches!(self, OpKind::Clear | OpKind::Pocket | OpKind::Adaptive | OpKind::Parallel | OpKind::Waterline | OpKind::Scallop | OpKind::Pencil)
    }

    /// Whether a tool kind suits the operation.
    pub fn accepts(self, k: ToolKind) -> bool {
        match self {
            OpKind::Face => matches!(k, ToolKind::Flat | ToolKind::Bull | ToolKind::FaceMill),
            OpKind::Clear | OpKind::Pocket => matches!(k, ToolKind::Flat | ToolKind::Bull | ToolKind::Ball | ToolKind::FaceMill),
            OpKind::Contour => matches!(k, ToolKind::Flat | ToolKind::Bull | ToolKind::Ball | ToolKind::TaperedBall | ToolKind::Slot),
            OpKind::Chamfer => matches!(k, ToolKind::Chamfer | ToolKind::Engrave | ToolKind::SpotDrill | ToolKind::CenterDrill),
            OpKind::Drill => matches!(k, ToolKind::Drill | ToolKind::SpotDrill | ToolKind::CenterDrill | ToolKind::Reamer | ToolKind::Flat),
            OpKind::Bore => matches!(k, ToolKind::Flat | ToolKind::Bull | ToolKind::Ball),
            OpKind::Thread => k == ToolKind::ThreadMill,
            OpKind::Tap => k == ToolKind::Tap,
            OpKind::Adaptive => matches!(k, ToolKind::Flat | ToolKind::Bull | ToolKind::Ball),
            OpKind::Parallel | OpKind::Waterline | OpKind::Scallop | OpKind::Pencil => matches!(k, ToolKind::Flat | ToolKind::Bull | ToolKind::Ball | ToolKind::TaperedBall),
            OpKind::Deburr => matches!(k, ToolKind::Chamfer | ToolKind::Engrave | ToolKind::SpotDrill | ToolKind::CenterDrill),
            OpKind::Trace => matches!(k, ToolKind::Flat | ToolKind::Ball | ToolKind::Bull | ToolKind::TaperedBall | ToolKind::Chamfer | ToolKind::Engrave),
            OpKind::VCarve => matches!(k, ToolKind::Engrave | ToolKind::Chamfer),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    #[default]
    Outside,
    Inside,
    Both,
}

/// One machining operation. Heights are in work coordinates (relative to the setup's origin).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Operation {
    pub id: u64,
    pub name: String,
    pub kind: OpKind,
    pub tool: u64,
    pub suppressed: bool,
    /// Top of the cut; None = top of the stock.
    pub top: Option<f64>,
    /// Bottom of the cut; None = found from the part (its top for facing, its bottom for contours,
    /// pocket floors, hole bottoms).
    pub bottom: Option<f64>,
    /// Depth per pass; None = the tool's.
    pub stepdown: Option<f64>,
    /// Stepover as a fraction of the tool diameter; None = the tool's.
    pub stepover: Option<f64>,
    /// Material left on walls.
    pub leave: f64,
    /// Material left on floors.
    pub leave_floor: f64,
    /// Climb milling (with a clockwise spindle); false for conventional.
    pub climb: bool,
    pub side: Side,
    /// Ramp angle into material, degrees.
    pub ramp_angle: f64,
    /// Holding tabs on contours: count, width and height (mm).
    pub tabs: u32,
    pub tab_width: f64,
    pub tab_height: f64,
    /// Only the pocket (or holes) around this XY point in work coordinates.
    pub at: Option<[f64; 2]>,
    /// Explicit hole positions in work coordinates; empty = the holes found in the part.
    pub points: Vec<[f64; 2]>,
    /// Hole diameter range to use; None = suited to the tool.
    pub hole_min: Option<f64>,
    pub hole_max: Option<f64>,
    /// Peck depth for drilling (0 = none).
    pub peck: f64,
    /// Dwell at the bottom of holes, seconds.
    pub dwell: f64,
    /// Extra depth through the bottom of through holes and cut-outs.
    pub breakthrough: f64,
    /// Chamfer width.
    pub chamfer: f64,
    /// Thread major diameter and pitch; None = from the hole and tool.
    pub thread_diameter: Option<f64>,
    pub pitch: Option<f64>,
    pub left_hand: bool,
    /// Radial depth of a slot mill cut into the wall.
    pub slot_depth: f64,
    /// Coolant on (overrides the tool's setting when true).
    pub coolant: bool,
    /// Adaptive: radius of the cycloidal loops, mm (0 = 40% of the tool diameter).
    pub trochoid: f64,
    /// Parallel: direction of the passes, degrees from X.
    pub angle: f64,
    /// 3D finishing: only where the surface slopes between these angles (degrees from horizontal).
    pub slope_min: f64,
    pub slope_max: f64,
    /// Deburr: points (work coordinates) on the edges to chamfer; empty = every sharp edge.
    pub edges: Vec<[f64; 3]>,
    /// Deburr: edges sharper than this angle between faces, degrees.
    pub edge_angle: f64,
    /// Rest machining: only cut where the earlier operations (and setups) left material.
    pub rest: bool,
    /// Pencil: diameter of the bigger tool whose leftovers to clean up (0 = the largest tool used before it).
    pub ref_diameter: f64,
    /// Trace and V-carve: the sketches (feature ids in the Part Studio) whose curves to follow.
    pub sketches: Vec<u64>,
    /// Trace: depth below the surface; V-carve: deepest cut (0 = as deep as the V-bit goes).
    pub depth: f64,
    /// Contour: radius of the arcs that lead onto and off the wall (0 = ramp straight along it).
    pub lead: f64,
    /// Adaptive: raise the feed for radial chip thinning (the loops take a thin cut, so the chips are thinner
    /// than the feed per tooth); capped at ×1.5, see `chip_thinning`.
    #[serde(skip_serializing_if = "is_false")]
    pub chip_thinning: bool,
}

impl Default for Operation {
    fn default() -> Self {
        Operation {
            id: 0,
            name: String::new(),
            kind: OpKind::Contour,
            tool: 0,
            suppressed: false,
            top: None,
            bottom: None,
            stepdown: None,
            stepover: None,
            leave: 0.0,
            leave_floor: 0.0,
            climb: true,
            side: Side::Outside,
            ramp_angle: 3.0,
            tabs: 0,
            tab_width: 6.0,
            tab_height: 2.0,
            at: None,
            points: vec![],
            hole_min: None,
            hole_max: None,
            peck: 0.0,
            dwell: 0.0,
            breakthrough: 0.5,
            chamfer: 0.5,
            thread_diameter: None,
            pitch: None,
            left_hand: false,
            slot_depth: 0.0,
            coolant: false,
            trochoid: 0.0,
            angle: 0.0,
            slope_min: 0.0,
            slope_max: 90.0,
            edges: vec![],
            edge_angle: 30.0,
            rest: false,
            ref_diameter: 0.0,
            sketches: vec![],
            depth: 0.0,
            lead: 0.0,
            chip_thinning: false,
        }
    }
}

/// Round to 0.1 mm.
fn fmt_round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

// ---------------------------------------------------------------- setup

/// Where the work coordinate origin (G54 zero) is.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Top of the stock, front-left corner.
    #[default]
    StockTopCorner,
    StockTopCenter,
    StockBottomCorner,
    /// Top of the part, front-left corner of its bounding box.
    PartTopCorner,
    /// The part's own origin.
    Model,
}

impl Origin {
    pub const ALL: [Origin; 5] = [Origin::StockTopCorner, Origin::StockTopCenter, Origin::StockBottomCorner, Origin::PartTopCorner, Origin::Model];
    pub fn label(self) -> &'static str {
        match self {
            Origin::StockTopCorner => "Stock top, front-left corner",
            Origin::StockTopCenter => "Stock top, centre",
            Origin::StockBottomCorner => "Stock bottom, front-left corner",
            Origin::PartTopCorner => "Part top, front-left corner",
            Origin::Model => "Part origin",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Origin::StockTopCorner => "stock_top_corner",
            Origin::StockTopCenter => "stock_top_center",
            Origin::StockBottomCorner => "stock_bottom_corner",
            Origin::PartTopCorner => "part_top_corner",
            Origin::Model => "model",
        }
    }
    pub fn parse(s: &str) -> Option<Origin> {
        Origin::ALL.into_iter().find(|o| o.key() == s.trim())
    }
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero2(t: &[f64; 2]) -> bool {
    t[0] == 0.0 && t[1] == 0.0
}

/// The `tilt` (about X, then Y, degrees) that turns part direction `axis` up onto the spindle from the "top" orientation.
pub fn tilt_for_axis(axis: [f64; 3]) -> [f64; 2] {
    let (x, y, z) = (axis[0], axis[1], axis[2]);
    let a = y.atan2(z);
    let z1 = (y * y + z * z).sqrt();
    let b = (-x).atan2(z1);
    [a.to_degrees(), b.to_degrees()]
}

/// Which way the part faces the spindle: the part direction that points up (+Z) on the machine.
pub const ORIENTS: [&str; 6] = ["top", "bottom", "front", "back", "left", "right"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Stock {
    /// Extra stock around the part's bounding box: sides (X and Y), top and bottom.
    pub side: f64,
    pub top: f64,
    pub bottom: f64,
    /// Fixed stock size (X, Y, Z) centred on the part in X/Y and sitting `bottom` below it; overrides the margins.
    pub size: Option<[f64; 3]>,
    /// Stock material, for feeds and speeds.
    pub material: String,
    /// For a setup that carries on from another: that setup's stock box in part coordinates
    /// (kept up to date from it; overrides the margins and size).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carried: Option<[[f64; 3]; 2]>,
}

impl Default for Stock {
    fn default() -> Self {
        Stock { side: 2.0, top: 1.0, bottom: 0.0, size: None, material: "Aluminium".into(), carried: None }
    }
}

/// A milling machine: work envelope and limits.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Machine {
    pub name: String,
    /// Travel along X, Y and Z, mm.
    pub travel: [f64; 3],
    /// Highest feed, mm/min.
    pub max_feed: f64,
    /// Rapid rate along Z when it differs from X/Y (0 = the same).
    pub rapid_z: f64,
    /// Automatic tool changer; without one the program pauses (M0) for each tool change.
    pub tool_changer: bool,
    /// Seconds to wait after starting the spindle before cutting (a VFD spindle needs a few to come up to speed).
    pub spin_up: f64,
}

impl Default for Machine {
    fn default() -> Self {
        Machine { name: String::new(), travel: [0.0; 3], max_feed: 0.0, rapid_z: 0.0, tool_changer: false, spin_up: 5.0 }
    }
}

/// Something holding the work that the tool must keep clear of (a clamp, a vise jaw, a screw head),
/// as a box in work coordinates (relative to the work zero).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Fixture {
    pub name: String,
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture { name: "Clamp".into(), min: [0.0, 0.0, 0.0], max: [20.0, 40.0, 12.0] }
    }
}

/// Fixture presets: name and size (X, Y, Z).
pub const FIXTURES: [(&str, [f64; 3]); 5] = [
    ("Toe clamp", [20.0, 40.0, 12.0]),
    ("Strap clamp", [25.0, 60.0, 20.0]),
    ("Screw head", [10.0, 10.0, 6.0]),
    ("Vise jaw", [150.0, 20.0, 40.0]),
    ("Bolt and washer", [24.0, 24.0, 15.0]),
];

/// How the G-code measures each tool after it is loaded.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Probe {
    /// "off", "plate" (a touch plate on the work sets the work Z zero) or "setter" (a fixed tool setter
    /// measures each tool's length against the first one; needs LinuxCNC, Mach3 or Fanuc).
    pub mode: String,
    /// Where the touch plate lies (work X, Y) and the work Z of the surface under it.
    pub plate_at: [f64; 3],
    pub plate_thickness: f64,
    /// Tool setter position in machine coordinates (G53): X, Y and the Z to start probing from.
    pub setter: [f64; 3],
    /// How far to probe down (for the touch plate from the top of the Z travel; for the setter from
    /// its start Z), and the fast and slow probing feeds (mm/min).
    pub travel: f64,
    pub fast: f64,
    pub slow: f64,
}

impl Default for Probe {
    fn default() -> Self {
        Probe { mode: "off".into(), plate_at: [0.0, 0.0, 0.0], plate_thickness: 15.0, setter: [0.0, 0.0, -10.0], travel: 100.0, fast: 300.0, slow: 25.0 }
    }
}

/// G-code the user adds: at the start (after the units and work offset), in place of the standard
/// tool change, and at the end (before M30). Placeholders: {tool} {name} {desc} {diameter} {rpm} {program} {setup}.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PostText {
    pub header: String,
    pub tool_change: String,
    pub footer: String,
}

/// A fixture preset placed against the stock (`lo`, `hi` in setup coordinates): vise jaws along the
/// front and back faces, clamps and screws over the edges in turn (left, right, front, back), each
/// overlapping the stock by a quarter of its size. Returned in work coordinates.
pub fn place_fixture(name: &str, size: [f64; 3], (lo, hi): (glam::DVec3, glam::DVec3), origin: glam::DVec3, n: usize) -> Fixture {
    use glam::DVec3;
    let s = DVec3::from(size);
    let mid = (lo + hi) / 2.0;
    let (a, b) = if name.contains("jaw") {
        let y = if n % 2 == 0 { lo.y - s.y } else { hi.y };
        (DVec3::new(mid.x - s.x / 2.0, y, lo.z), DVec3::new(mid.x + s.x / 2.0, y + s.y, lo.z + s.z))
    } else {
        // lies on top of the stock, over an edge
        let (w, d) = if n % 4 < 2 { (s.x, s.y) } else { (s.y, s.x) };
        let (x, y) = match n % 4 {
            0 => (lo.x - w * 0.75, mid.y - d / 2.0),
            1 => (hi.x - w * 0.25, mid.y - d / 2.0),
            2 => (mid.x - w / 2.0, lo.y - d * 0.75),
            _ => (mid.x - w / 2.0, hi.y - d * 0.25),
        };
        (DVec3::new(x, y, hi.z), DVec3::new(x + w, y + d, hi.z + s.z))
    };
    let r = |v: DVec3| [(v.x * 100.0).round() / 100.0, (v.y * 100.0).round() / 100.0, (v.z * 100.0).round() / 100.0];
    Fixture { name: name.into(), min: r(a - origin), max: r(b - origin) }
}

/// Soft jaws: vise jaw blanks with a pocket machined to the shape of what the setup holds (see cam_jaws).
/// The vise closes along Y: one jaw in front of the part, one behind.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SoftJaws {
    /// Jaw blank size: along the vise (X), front to back (Y) and height (Z).
    pub width: f64,
    pub thickness: f64,
    pub height: f64,
    /// How deep the part sits in the jaws.
    pub grip: f64,
    /// Opening between the jaw faces while the pocket is cut (the spacer between them).
    pub gap: f64,
    /// Room around the part in the pocket.
    pub clearance: f64,
    /// Diameter of the end mill that cuts the pocket: corners it can't reach get reliefs for it.
    pub cutter: f64,
    /// Jaw material, for feeds and speeds.
    pub material: String,
    /// 3D printed instead of machined: no reliefs for a cutter (the printer makes sharp corners), and the
    /// jaws go to a Part Studio to export (STEP) rather than to a machining setup.
    pub printed: bool,
    /// 3D printed as one block instead of two jaws: it goes in between the vise jaws, sits on the vise bed and
    /// is squeezed round the part (the gap is filled in).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub one_piece: bool,
    /// How the jaws bolt into the vise: screw holes, and the vise steel around them (None: plain blanks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<JawMount>,
}

/// Screw holes that hold soft jaws in the vise, and the vise around them. Holes run down through the
/// jaw, counterbored from the top, the same on both jaws.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct JawMount {
    pub name: String,
    /// Hole centres along the jaw (X), from the middle of the jaw.
    pub holes: Vec<f64>,
    /// Hole centres from the jaw's grip face (the face toward the part).
    pub inset: f64,
    /// Through hole and counterbore diameters.
    pub drill: f64,
    pub cbore: f64,
    /// Material left under the counterbore for the screw head to seat on, and the head's height.
    pub seat: f64,
    pub head: f64,
    /// Front-to-back size the vise takes (a channel the jaw sits in); 0: any.
    pub fit: f64,
    /// The vise steel the jaws sit in: a lip in front of each jaw (toward the part) `lip` deep and
    /// `lip_height` above the jaw bottom, and the vise jaw behind it, `back` deep and `back_height` high.
    pub lip: f64,
    pub lip_height: f64,
    pub back: f64,
    pub back_height: f64,
}

impl Default for JawMount {
    fn default() -> Self {
        JawMount::gen3_insert_bar()
    }
}

impl JawMount {
    /// In place of a Gen3 Smooth Insert Bar (95.5 × 12.7 × 8.7, in a 12.7 wide channel 4.57 deep, held by
    /// four screws on a 25.4 pitch) on an M12 dual-station vise's fixed side and top jaws.
    pub fn gen3_insert_bar() -> JawMount {
        JawMount {
            name: "Gen3 insert bar".into(),
            holes: vec![-38.1, -12.7, 12.7, 38.1],
            inset: 6.35,
            drill: 5.25,
            cbore: 8.74,
            seat: 4.13,
            // #10 socket head
            head: 4.83,
            fit: 12.7,
            lip: 1.9,
            lip_height: 4.57,
            back: 33.0,
            back_height: 6.22,
        }
    }

    /// In place of an M12 adjustable base's Top Jaw (95.5 along the vise, 48 deep), held by two M8 socket
    /// head screws 56.4 apart, 29.6 back from the grip face.
    pub fn top_jaw() -> JawMount {
        JawMount {
            name: "Top jaw".into(),
            holes: vec![-28.2, 28.2],
            inset: 29.6,
            drill: 9.0,
            cbore: 14.0,
            seat: 6.0,
            head: 8.0,
            fit: 0.0,
            lip: 0.0,
            lip_height: 0.0,
            back: 0.0,
            back_height: 0.0,
        }
    }
}

/// Jaw mounting presets: name, key for the agent, and the blank (along the vise, front to back) it comes with.
pub const JAW_MOUNTS: [(&str, &str, f64, f64); 2] = [("Gen3 insert bar", "gen3_insert_bar", 95.5, 12.7), ("Top jaw", "top_jaw", 95.5, 48.0)];

/// A mounting preset by name or key, with its blank length and thickness.
pub fn jaw_mount(name: &str) -> Option<(JawMount, f64, f64)> {
    let n = name.trim();
    let m = JAW_MOUNTS.iter().find(|m| m.0.eq_ignore_ascii_case(n) || m.1.eq_ignore_ascii_case(n))?;
    Some((if m.1 == "top_jaw" { JawMount::top_jaw() } else { JawMount::gen3_insert_bar() }, m.2, m.3))
}

impl Default for SoftJaws {
    fn default() -> Self {
        SoftJaws { width: 150.0, thickness: 25.0, height: 40.0, grip: 6.0, gap: 4.0, clearance: 0.05, cutter: 6.0, material: "Aluminium".into(), printed: false, one_piece: false, mount: None }
    }
}

/// Machine presets: name, travel, max rpm, max feed, rapid XY, rapid Z, tool changer.
pub const MACHINES: [(&str, [f64; 3], f64, f64, f64, f64, bool); 10] = [
    ("Desktop router (3018 class)", [300.0, 180.0, 45.0], 10000.0, 1000.0, 1500.0, 600.0, false),
    ("Hobby router, 600 × 400", [600.0, 400.0, 100.0], 24000.0, 5000.0, 6000.0, 3000.0, false),
    ("Large router, 1200 × 1200", [1220.0, 1220.0, 150.0], 24000.0, 10000.0, 15000.0, 5000.0, false),
    ("Shapeoko 4 XXL", [838.0, 838.0, 95.0], 30000.0, 5000.0, 10000.0, 3000.0, false),
    ("Carbide 3D Nomad 3", [203.0, 203.0, 76.0], 24000.0, 2540.0, 2540.0, 2540.0, false),
    ("Tormach PCNC 440", [254.0, 159.0, 254.0], 10000.0, 2540.0, 3430.0, 3430.0, true),
    ("Tormach 770M", [356.0, 191.0, 330.0], 10000.0, 3430.0, 3430.0, 3430.0, true),
    ("Haas Mini Mill", [406.0, 305.0, 254.0], 6000.0, 12700.0, 15200.0, 15200.0, true),
    ("Haas VF-2", [762.0, 406.0, 508.0], 8100.0, 16500.0, 25400.0, 25400.0, true),
    ("Generic VMC", [500.0, 400.0, 400.0], 12000.0, 10000.0, 24000.0, 24000.0, true),
];

/// Apply a machine preset to a setup by name.
pub fn set_machine(c: &mut Cam, name: &str) -> bool {
    let Some(m) = MACHINES.iter().find(|m| m.0.eq_ignore_ascii_case(name.trim())) else { return false };
    c.machine = Machine { name: m.0.into(), travel: m.1, max_feed: m.3, rapid_z: m.5, tool_changer: m.6, spin_up: c.machine.spin_up };
    c.max_rpm = m.2;
    c.rapid_rate = m.4;
    true
}

/// G-code dialects.
pub const POSTS: [(&str, &str); 4] = [("grbl", "GRBL"), ("linuxcnc", "LinuxCNC"), ("fanuc", "Fanuc / Haas"), ("mach3", "Mach3 / Mach4")];

/// A Manufacture tab: one setup with its tools and operations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Cam {
    pub name: String,
    /// Part Studio the parts come from.
    pub studio: usize,
    /// Parts to machine; empty = every visible part of the studio.
    pub bodies: Vec<BodyId>,
    /// One of ORIENTS.
    pub orient: String,
    /// 3+2 tilt after `orient`, degrees: about the setup's X axis, then its Y axis (angle blocks,
    /// tilting vises, a bore at a compound angle). Zero for a plain setup.
    #[serde(skip_serializing_if = "is_zero2")]
    pub tilt: [f64; 2],
    pub stock: Stock,
    pub origin: Origin,
    /// Rapid height above the stock top.
    pub clearance: f64,
    /// Height above the material where rapids stop and feeds start.
    pub retract: f64,
    /// Machine rapid rate (mm/min), for the cycle time.
    pub rapid_rate: f64,
    pub max_rpm: f64,
    /// Slowest useful spindle speed (a high-speed spindle loses torque below it); 0 = no limit.
    /// Suggested speeds stay above it.
    #[serde(skip_serializing_if = "is_zero")]
    pub min_rpm: f64,
    /// Post processor key from POSTS.
    pub post: String,
    pub program: u32,
    /// The machine it runs on (travel, feed limit, tool changer).
    pub machine: Machine,
    /// Coolant for the tools that don't set their own (`Auto`: flood for metal stock, off otherwise).
    #[serde(deserialize_with = "coolant_or_bool")]
    pub coolant: Coolant,
    /// Name of the setup (Manufacture tab) whose leftover stock this one starts from; empty = fresh stock.
    /// Use it to flip the part and machine the other side.
    pub follows: String,
    /// Write arcs (G2/G3) where the toolpath runs along circles, instead of many short lines.
    pub arcs: bool,
    /// Clamps, vise jaws, screws: boxes the tool keeps clear of (toolpaths lift over them).
    pub fixtures: Vec<Fixture>,
    /// Hold the part in soft jaws shaped to it (they act as fixtures).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_jaws: Option<SoftJaws>,
    /// Name of the setup whose soft jaws this one machines (instead of the Part Studio's parts).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub jaws_of: String,
    /// Measuring tools after a change: touch plate or tool setter.
    pub probe: Probe,
    /// The user's own G-code for the start, tool changes and end of the program.
    pub post_text: PostText,
    pub tools: Vec<Tool>,
    pub ops: Vec<Operation>,
    pub next_id: u64,
}

impl Default for Cam {
    fn default() -> Self {
        Cam {
            name: "Manufacture 1".into(),
            studio: 0,
            bodies: vec![],
            orient: "top".into(),
            tilt: [0.0; 2],
            stock: Stock::default(),
            origin: Origin::default(),
            clearance: 10.0,
            retract: 2.0,
            rapid_rate: 3000.0,
            max_rpm: 24000.0,
            min_rpm: 0.0,
            post: "grbl".into(),
            program: 1000,
            machine: Machine::default(),
            coolant: Coolant::Auto,
            follows: String::new(),
            arcs: true,
            fixtures: vec![],
            soft_jaws: None,
            jaws_of: String::new(),
            probe: Probe::default(),
            post_text: PostText::default(),
            tools: vec![],
            ops: vec![],
            next_id: 1,
        }
    }
}

impl Cam {
    pub fn new_id(&mut self) -> u64 {
        self.next_id = self.next_id.max(1);
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn tool(&self, id: u64) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }

    /// Add a tool (a copy of `t`), giving it an id and the next free tool number when its own is taken.
    pub fn add_tool(&mut self, mut t: Tool) -> u64 {
        t.id = self.new_id();
        if t.number == 0 || self.tools.iter().any(|x| x.number == t.number) {
            t.number = self.tools.iter().map(|x| x.number).max().unwrap_or(0) + 1;
        }
        let id = t.id;
        self.tools.push(t);
        id
    }

    /// Add an operation with sensible defaults for its kind; picks a suitable tool when `tool` is 0.
    pub fn add_op(&mut self, kind: OpKind, tool: u64) -> u64 {
        let tool = if tool != 0 { tool } else { self.tools.iter().find(|t| kind.accepts(t.kind)).map(|t| t.id).unwrap_or(0) };
        let id = self.new_id();
        let n = self.ops.iter().filter(|o| o.kind == kind).count() + 1;
        let mut op = Operation { id, name: format!("{} {n}", kind.label()), kind, tool, ..Default::default() };
        if kind == OpKind::Chamfer {
            op.side = Side::Both;
        }
        if kind == OpKind::Clear {
            op.leave = 0.3;
            op.leave_floor = 0.2;
        }
        // roughing after other roughing only cuts what is left
        if matches!(kind, OpKind::Clear | OpKind::Adaptive | OpKind::Pocket)
            && self.ops.iter().any(|o| matches!(o.kind, OpKind::Clear | OpKind::Adaptive | OpKind::Pocket | OpKind::Face))
        {
            op.rest = true;
        }
        let tl = self.tool(tool).cloned();
        match kind {
            OpKind::Adaptive => {
                op.leave = 0.3;
                op.leave_floor = 0.2;
                op.ramp_angle = 2.0;
                op.stepover = Some(0.1);
                if let Some(t) = &tl {
                    op.stepdown = Some(fmt_round((t.diameter * 1.5).min(t.flute_length * 0.9).max(t.stepdown)));
                }
            }
            OpKind::Parallel | OpKind::Scallop => op.stepover = Some(0.1),
            OpKind::Waterline => op.stepdown = Some(0.3),
            OpKind::Deburr => op.chamfer = 0.3,
            OpKind::Pencil => {
                op.stepover = Some(0.1);
                op.leave = 0.0;
            }
            OpKind::Trace => {
                op.side = Side::Both;
                op.depth = 0.5;
            }
            OpKind::VCarve => op.stepover = Some(0.02),
            _ => {}
        }
        if kind == OpKind::Drill && self.tool(tool).map(|t| t.kind == ToolKind::Drill && t.diameter > 0.0).unwrap_or(false) {
            op.peck = self.tool(tool).map(|t| t.diameter).unwrap_or(0.0);
        }
        self.ops.push(op);
        id
    }

    pub fn op(&self, id: u64) -> Option<&Operation> {
        self.ops.iter().find(|o| o.id == id)
    }

    pub fn op_mut(&mut self, id: u64) -> Option<&mut Operation> {
        self.ops.iter_mut().find(|o| o.id == id)
    }

    /// The speed a tool really runs at: its own, capped at the spindle's maximum.
    pub fn spindle_rpm(&self, t: &Tool) -> f64 {
        if self.max_rpm > 0.0 && t.rpm > self.max_rpm {
            self.max_rpm
        } else {
            t.rpm
        }
    }

    /// The coolant an operation runs with: the operation's (when it asks for it), else the tool's, else the setup's.
    pub fn coolant_for(&self, t: &Tool, op: Option<&Operation>) -> Coolant {
        let setup = match self.coolant {
            Coolant::Auto if is_metal(&self.stock.material) => Coolant::Flood,
            Coolant::Auto => Coolant::Off,
            c => c,
        };
        let c = if t.coolant == Coolant::Auto { setup } else { t.coolant };
        if !c.is_on() && op.map(|o| o.coolant).unwrap_or(false) {
            Coolant::Flood
        } else {
            c
        }
    }

    /// What the operator does before running the program: how the part sits (the side up, a 3+2 tilt) and
    /// what holds it. Plain lines for the G-code header and the setup sheet.
    pub fn setup_notes(&self) -> Vec<String> {
        let mut v = vec![];
        if !self.jaws_of.is_empty() {
            v.push(format!("clamp both soft jaw blanks of {} in the vise on a spacer, tops level", self.jaws_of));
        } else if !self.follows.is_empty() {
            v.push(format!("carries on from {}: clamp the part it left with its {} side up", self.follows, self.orient));
        } else if self.orient != "top" {
            v.push(format!("clamp the part with its {} side up", self.orient));
        }
        if self.tilt != [0.0; 2] {
            let mut t = vec![];
            if self.tilt[0] != 0.0 {
                t.push(format!("{}° about X ({} edge raised)", fmt_mm(self.tilt[0].abs()), if self.tilt[0] > 0.0 { "back" } else { "front" }));
            }
            if self.tilt[1] != 0.0 {
                t.push(format!("{}° about Y ({} edge raised)", fmt_mm(self.tilt[1].abs()), if self.tilt[1] > 0.0 { "left" } else { "right" }));
            }
            v.push(format!("3+2 setup: tilt the part {} on an angle block or tilting vise before running", t.join(", then ")));
        }
        if let Some(sj) = &self.soft_jaws {
            v.push(format!("hold the part {} mm deep in the soft jaws (vise closing along Y)", fmt_mm(sj.grip)));
        }
        for f in &self.fixtures {
            v.push(format!(
                "fixture {}: X{} to {}, Y{} to {}, Z{} to {} (work)",
                f.name,
                fmt_mm(f.min[0]),
                fmt_mm(f.max[0]),
                fmt_mm(f.min[1]),
                fmt_mm(f.max[1]),
                fmt_mm(f.min[2]),
                fmt_mm(f.max[2])
            ));
        }
        v
    }
}

/// Metal stock, which wants coolant.
pub fn is_metal(material: &str) -> bool {
    !["Plastic", "Hardwood", "Softwood / MDF", "Foam"].iter().any(|m| m.eq_ignore_ascii_case(material.trim()))
}

/// Feed factor for radial chip thinning at a radial engagement of `ae` × the tool diameter: the chip is thinner
/// than the feed per tooth when the tool takes less than half its width. Capped at 1.5 to stay on the safe side.
pub fn chip_thinning(ae: f64) -> f64 {
    let a = ae.clamp(0.01, 1.0);
    if a >= 0.5 {
        1.0
    } else {
        (1.0 / (2.0 * (a * (1.0 - a)).sqrt())).min(1.5)
    }
}

// ---------------------------------------------------------------- saved machine

/// The machine side of a setup: controller, spindle, machine limits, coolant, probing and custom G-code.
/// New setups start from the document's last setup or from the one the user saved.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MachineSettings {
    pub post: String,
    pub max_rpm: f64,
    pub min_rpm: f64,
    pub rapid_rate: f64,
    pub clearance: f64,
    pub retract: f64,
    pub arcs: bool,
    pub coolant: Coolant,
    pub machine: Machine,
    pub probe: Probe,
    pub post_text: PostText,
}

impl Default for MachineSettings {
    fn default() -> Self {
        MachineSettings::of(&Cam::default())
    }
}

impl MachineSettings {
    pub fn of(c: &Cam) -> MachineSettings {
        MachineSettings {
            post: c.post.clone(),
            max_rpm: c.max_rpm,
            min_rpm: c.min_rpm,
            rapid_rate: c.rapid_rate,
            clearance: c.clearance,
            retract: c.retract,
            arcs: c.arcs,
            coolant: c.coolant,
            machine: c.machine.clone(),
            probe: c.probe.clone(),
            post_text: c.post_text.clone(),
        }
    }

    pub fn apply(&self, c: &mut Cam) {
        c.post = self.post.clone();
        c.max_rpm = self.max_rpm;
        c.min_rpm = self.min_rpm;
        c.rapid_rate = self.rapid_rate;
        c.clearance = self.clearance;
        c.retract = self.retract;
        c.arcs = self.arcs;
        c.coolant = self.coolant;
        c.machine = self.machine.clone();
        c.probe = self.probe.clone();
        c.post_text = self.post_text.clone();
    }
}

fn machine_path() -> Option<std::path::PathBuf> {
    library_path().map(|p| p.with_file_name("machine.json"))
}

/// The machine the user saved (~/.config/cad-rust-app/machine.json).
pub fn load_machine() -> Option<MachineSettings> {
    machine_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok())
}

pub fn save_machine(m: &MachineSettings) -> Result<std::path::PathBuf, String> {
    let p = machine_path().ok_or("no config folder (HOME isn't set)")?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&p, serde_json::to_string_pretty(m).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(p)
}

/// A new setup: its machine settings come from the document's last setup, else from `saved`, else the defaults.
pub fn new_setup(name: String, studio: usize, cams: &[Cam], saved: Option<MachineSettings>) -> Cam {
    let mut c = Cam { name, studio, ..Default::default() };
    if let Some(m) = cams.iter().rev().find(|c| c.jaws_of.is_empty()).map(MachineSettings::of).or(saved) {
        m.apply(&mut c);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coolant_reads_old_files_and_follows_the_setup() {
        // older files keep coolant as true/false
        let t: Tool = serde_json::from_str(r#"{"coolant": true}"#).unwrap();
        assert_eq!(t.coolant, Coolant::Flood);
        let t: Tool = serde_json::from_str(r#"{"coolant": false}"#).unwrap();
        assert_eq!(t.coolant, Coolant::Auto);
        let t: Tool = serde_json::from_str(r#"{"coolant": "mist"}"#).unwrap();
        assert_eq!(t.coolant, Coolant::Mist);
        let back: Tool = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        // a tool that leaves it to the setup: flood on metal, off on wood, or what the setup says
        let mut c = Cam::default();
        let t = Tool::default();
        assert_eq!(c.coolant_for(&t, None), Coolant::Flood);
        c.stock.material = "Hardwood".into();
        assert_eq!(c.coolant_for(&t, None), Coolant::Off);
        let op = Operation { coolant: true, ..Default::default() };
        assert_eq!(c.coolant_for(&t, Some(&op)), Coolant::Flood);
        c.coolant = Coolant::Mist;
        assert_eq!(c.coolant_for(&t, None), Coolant::Mist);
        assert_eq!(c.coolant_for(&Tool { coolant: Coolant::Off, ..Default::default() }, None), Coolant::Off);
    }

    #[test]
    fn new_setups_keep_the_machine() {
        let saved = MachineSettings { post: "mach3".into(), min_rpm: 12000.0, machine: Machine { spin_up: 6.0, ..Default::default() }, ..Default::default() };
        // an empty document: the saved machine
        let c = new_setup("A".into(), 0, &[], Some(saved.clone()));
        assert_eq!((c.post.as_str(), c.min_rpm, c.machine.spin_up, c.machine.tool_changer), ("mach3", 12000.0, 6.0, false));
        // otherwise the last setup's
        let mut prev = Cam { post: "linuxcnc".into(), max_rpm: 18000.0, ..Default::default() };
        prev.machine.tool_changer = true;
        let c = new_setup("B".into(), 0, &[prev], Some(saved));
        assert_eq!((c.post.as_str(), c.max_rpm, c.machine.tool_changer, c.name.as_str()), ("linuxcnc", 18000.0, true, "B"));
        assert!(c.tools.is_empty() && c.ops.is_empty());
        // nothing to go on: GRBL defaults, no tool changer
        assert!(!new_setup("C".into(), 0, &[], None).machine.tool_changer);
    }

    #[test]
    fn chip_thinning_is_capped() {
        assert_eq!(chip_thinning(0.5), 1.0);
        assert!((chip_thinning(0.2) - 1.25).abs() < 1e-9);
        assert_eq!(chip_thinning(0.1), 1.5);
    }
}
