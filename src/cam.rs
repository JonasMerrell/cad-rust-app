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
    pub coolant: bool,
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
            shank_diameter: 6.0,
            flutes: 2,
            pitch: 0.0,
            cutting_width: 0.0,
            neck_diameter: 0.0,
            material: "carbide".into(),
            coolant: false,
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

pub fn fmt_mm(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

// ---------------------------------------------------------------- feeds and speeds

/// Stock materials with a cutting speed (m/min, carbide) and a chip load factor.
pub const MATERIALS: [(&str, f64, f64); 8] = [
    ("Aluminium", 300.0, 1.0),
    ("Brass", 200.0, 0.9),
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
/// `max_rpm` is the spindle's limit.
pub fn suggest(tool: &mut Tool, stock: &str, max_rpm: f64) {
    let (vc, k) = material(stock);
    let d = tool.diameter.max(0.1);
    let hss = tool.material != "carbide";
    let vc = if hss { vc * 0.35 } else { vc };
    let rpm = (vc * 1000.0 / (std::f64::consts::PI * d)).min(max_rpm.max(1000.0)).max(1000.0);
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
            tool.rpm = (tool.rpm * 0.5 / 100.0).round().max(5.0) * 100.0;
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
        suggest(t, "Aluminium", 24000.0);
    }
    v
}

fn library_path() -> Option<std::path::PathBuf> {
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
}

impl OpKind {
    pub const ALL: [OpKind; 9] = [OpKind::Face, OpKind::Clear, OpKind::Pocket, OpKind::Contour, OpKind::Chamfer, OpKind::Drill, OpKind::Bore, OpKind::Thread, OpKind::Tap];

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
        }
    }

    pub fn parse(s: &str) -> Option<OpKind> {
        let s = s.trim().to_lowercase();
        OpKind::ALL.into_iter().find(|k| k.key() == s || k.label().to_lowercase() == s).or(match s.as_str() {
            "facing" => Some(OpKind::Face),
            "rough" | "roughing" | "adaptive" | "clearing" => Some(OpKind::Clear),
            "profile" | "outline" => Some(OpKind::Contour),
            "deburr" => Some(OpKind::Chamfer),
            "spot" | "ream" | "peck" => Some(OpKind::Drill),
            "helix" | "helical" | "circular" => Some(OpKind::Bore),
            "thread_mill" | "threadmill" => Some(OpKind::Thread),
            "tapping" => Some(OpKind::Tap),
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
        }
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
        }
    }
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
}

impl Default for Stock {
    fn default() -> Self {
        Stock { side: 2.0, top: 1.0, bottom: 0.0, size: None, material: "Aluminium".into() }
    }
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
    pub stock: Stock,
    pub origin: Origin,
    /// Rapid height above the stock top.
    pub clearance: f64,
    /// Height above the material where rapids stop and feeds start.
    pub retract: f64,
    /// Machine rapid rate (mm/min), for the cycle time.
    pub rapid_rate: f64,
    pub max_rpm: f64,
    /// Post processor key from POSTS.
    pub post: String,
    pub program: u32,
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
            stock: Stock::default(),
            origin: Origin::default(),
            clearance: 10.0,
            retract: 2.0,
            rapid_rate: 3000.0,
            max_rpm: 24000.0,
            post: "grbl".into(),
            program: 1000,
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
        if kind == OpKind::Drill && self.tool(tool).map(|t| t.kind == ToolKind::Drill && t.diameter > 0.0).unwrap_or(false) {
            op.peck = self.tool(tool).map(|t| t.diameter).unwrap_or(0.0);
        }
        self.ops.push(op);
        id
    }

    pub fn op(&self, id: u64) -> Option<&Operation> {
        self.ops.iter().find(|o| o.id == id)
    }

    #[cfg(test)]
    pub fn op_mut(&mut self, id: u64) -> Option<&mut Operation> {
        self.ops.iter_mut().find(|o| o.id == id)
    }
}
