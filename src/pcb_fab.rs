//! Output jobs for PCB tabs, like Altium's OutJob: one press writes every fabrication and
//! assembly file. Besides the Gerbers, drills, BOM and pick-and-place from `pcb_export`, this
//! makes a Gerber X2 job file, an IPC-D-356A netlist for bare-board test, a drill drawing, a
//! PDF of the schematic, assembly drawings and the drill drawing, and an ODB++ job.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::pcb::{self, Layer, Pcb, WPad};
use crate::pcb_export::{self, Draw, Prim};
use crate::pcb_lib::P;

/// Which outputs the job writes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OutJob {
    pub gerbers: bool,
    pub bom: bool,
    pub cpl: bool,
    pub ipc356: bool,
    pub drill_map: bool,
    pub pdf: bool,
    pub odb: bool,
}

impl Default for OutJob {
    fn default() -> Self {
        OutJob { gerbers: true, bom: true, cpl: true, ipc356: true, drill_map: true, pdf: true, odb: true }
    }
}

impl OutJob {
    pub fn is_default(&self) -> bool {
        *self == OutJob::default()
    }
    /// The outputs by name, for the agent and the sidebar.
    pub const NAMES: [(&'static str, &'static str); 7] = [
        ("gerbers", "Gerbers, drills and the X2 job file"),
        ("bom", "Bill of materials (CSV)"),
        ("cpl", "Pick and place (CSV)"),
        ("ipc356", "IPC-D-356A netlist for electrical test"),
        ("drill_map", "Drill drawing (Gerber)"),
        ("pdf", "PDF: schematic, assembly and drill drawings"),
        ("odb", "ODB++ job"),
    ];
    pub fn get_mut(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "gerbers" => &mut self.gerbers,
            "bom" => &mut self.bom,
            "cpl" => &mut self.cpl,
            "ipc356" => &mut self.ipc356,
            "drill_map" => &mut self.drill_map,
            "pdf" => &mut self.pdf,
            "odb" => &mut self.odb,
            _ => return None,
        })
    }
}

fn base_name(p: &Pcb) -> String {
    let b: String = p.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    if b.is_empty() { "board".into() } else { b }
}

struct Prep {
    p: Pcb,
    nl: pcb::Netlist,
    pads: Vec<WPad>,
}

fn prep(p: &Pcb) -> Prep {
    let mut p = p.clone();
    let nl = p.netlist();
    p.retag_copper(&nl);
    let pads = p.pads(&nl);
    Prep { p, nl, pads }
}

// ---------------------------------------------------------------- IPC-D-356A

fn net_name(x: &Prep, pad: &WPad) -> String {
    pad.net.map(|n| x.nl.nets[n].name.clone()).unwrap_or_else(|| "N/C".into())
}

/// The IPC-D-356A netlist: every pad and via as a test point with its net, in micrometres.
pub fn ipc356(p: &Pcb) -> String {
    let x = prep(p);
    let mut s = String::new();
    let _ = writeln!(s, "C  IPC-D-356A netlist made by rCAD");
    let _ = writeln!(s, "P  JOB   {}", x.p.name);
    let _ = writeln!(s, "P  CODE  00");
    let _ = writeln!(s, "P  UNITS CUST 1");
    let _ = writeln!(s, "P  DIM   N");
    let _ = writeln!(s, "P  VER   IPC-D-356A");
    let _ = writeln!(s, "P  IMAGE PRIMARY");
    let um = |v: f64| (v * 1000.0).round() as i64;
    // names longer than 14 characters get an alias (P  NNAME)
    let mut alias: Vec<(String, String)> = vec![];
    let mut short = |n: &str| -> String {
        if n.len() <= 14 {
            return n.to_string();
        }
        if let Some(a) = alias.iter().find(|a| a.0 == n) {
            return a.1.clone();
        }
        let a = format!("NNAME{}", alias.len() + 1);
        alias.push((n.to_string(), a.clone()));
        a
    };
    let mut rows = vec![];
    for pad in &x.pads {
        let part = &x.p.parts[pad.part];
        let tht = pad.drill > 0.0;
        let access = match pad.layer {
            None => 0,
            Some(Layer::Top) => 1,
            Some(Layer::Bottom) => 2,
        };
        let net = short(&net_name(&x, pad));
        let rot = ((pad.rot.rem_euclid(360.0)).round() as i64) % 360;
        let hole = if tht { format!("D{:04}P", um(pad.drill).min(9999)) } else { "      ".to_string() };
        rows.push(format!(
            "{}{:<14}   {:<6}-{:<4} {}A{:02}X{:+07}Y{:+07}X{:04}Y{:04}R{:03} S{}",
            if tht { "317" } else { "327" },
            net,
            part.reference.chars().take(6).collect::<String>(),
            pad.num.chars().take(4).collect::<String>(),
            hole,
            access,
            um(pad.at[0]),
            um(pad.at[1]),
            um(pad.size[0]).min(9999),
            um(pad.size[1]).min(9999),
            rot,
            if tht { 0 } else if access == 1 { 1 } else { 2 }
        ));
    }
    for v in &x.p.board.vias {
        let net = short(if v.net.is_empty() { "N/C" } else { &v.net });
        rows.push(format!("317{:<14}   VIA   -     D{:04}PA00X{:+07}Y{:+07}X{:04}Y{:04}R000 S3", net, um(v.drill).min(9999), um(v.at[0]), um(v.at[1]), um(v.d).min(9999), um(v.d).min(9999)));
    }
    for (full, a) in &alias {
        let _ = writeln!(s, "P  {a:<6}{full}");
    }
    for r in rows {
        let _ = writeln!(s, "{r}");
    }
    s += "999\n";
    s
}

// ---------------------------------------------------------------- Gerber job file

/// The Gerber X2 job file (.gbrjob): board size, thickness and the layer stack, so a fab reads
/// the set without guessing.
pub fn gbrjob(p: &Pcb, files: &[(String, String)]) -> String {
    let size = p.board.size();
    let function = |text: &str| -> Option<(String, String)> {
        let line = text.lines().find(|l| l.starts_with("%TF.FileFunction,"))?;
        let f = line.trim_start_matches("%TF.FileFunction,").trim_end_matches("*%").to_string();
        let pol = text.lines().find(|l| l.starts_with("%TF.FilePolarity,")).map(|l| l.trim_start_matches("%TF.FilePolarity,").trim_end_matches("*%").to_string()).unwrap_or_else(|| "Positive".into());
        Some((f, pol))
    };
    let mut layers = vec![];
    for (name, text) in files {
        if let Some((f, pol)) = function(text) {
            layers.push(serde_json::json!({"Path": name, "FileFunction": f, "FilePolarity": pol}));
        }
    }
    let job = serde_json::json!({
        "Header": {"GenerationSoftware": {"Vendor": "rCAD", "Application": "rCAD PCB", "Version": env!("CARGO_PKG_VERSION")}, "CreationDate": ""},
        "GeneralSpecs": {
            "ProjectId": {"Name": p.name, "GUID": "", "Revision": "1"},
            "Size": {"X": (size[0] * 1000.0).round() / 1000.0, "Y": (size[1] * 1000.0).round() / 1000.0},
            "LayerNumber": 2,
            "BoardThickness": p.board.thickness,
        },
        "DesignRules": [{"Layers": "Outer", "PadToPad": p.board.rules.clearance, "PadToTrack": p.board.rules.clearance, "TrackToTrack": p.board.rules.clearance, "MinLineWidth": p.board.rules.min_track}],
        "FilesAttributes": layers,
        "MaterialStackup": [
            {"Type": "Legend", "Name": "Top Silkscreen"},
            {"Type": "SolderMask", "Name": "Top Solder Mask", "Color": if p.board.color.is_empty() { "Green" } else { &p.board.color }},
            {"Type": "Copper", "Name": "F.Cu", "Thickness": 0.035},
            {"Type": "Dielectric", "Name": "Core", "Material": "FR4", "Thickness": (p.board.thickness - 0.07).max(0.1)},
            {"Type": "Copper", "Name": "B.Cu", "Thickness": 0.035},
            {"Type": "SolderMask", "Name": "Bottom Solder Mask"},
            {"Type": "Legend", "Name": "Bottom Silkscreen"},
        ],
    });
    serde_json::to_string_pretty(&job).unwrap_or_default()
}

// ---------------------------------------------------------------- drill drawing

/// Each hole size gets a symbol: circle, cross, square, triangle, diamond, X, then combinations.
fn drill_symbol(out: &mut Vec<Draw>, k: usize, c: P, r: f64, rgba: [u8; 4]) {
    let w = (r * 0.12).max(0.05);
    let seg = |out: &mut Vec<Draw>, a: P, b: P| out.push(Draw { rgba, prim: Prim::Seg { a, b, w } });
    let poly = |out: &mut Vec<Draw>, pts: &[P]| {
        for i in 0..pts.len() {
            seg(out, pts[i], pts[(i + 1) % pts.len()]);
        }
    };
    let at = |x: f64, y: f64| [c[0] + x * r, c[1] + y * r];
    match k % 6 {
        0 => out.push(Draw { rgba, prim: Prim::Circle { c, r, w, fill: false } }),
        1 => {
            seg(out, at(-1.0, 0.0), at(1.0, 0.0));
            seg(out, at(0.0, -1.0), at(0.0, 1.0));
        }
        2 => poly(out, &[at(-0.8, -0.8), at(0.8, -0.8), at(0.8, 0.8), at(-0.8, 0.8)]),
        3 => poly(out, &[at(0.0, 1.0), at(-0.87, -0.5), at(0.87, -0.5)]),
        4 => poly(out, &[at(0.0, 1.0), at(1.0, 0.0), at(0.0, -1.0), at(-1.0, 0.0)]),
        _ => {
            seg(out, at(-0.75, -0.75), at(0.75, 0.75));
            seg(out, at(-0.75, 0.75), at(0.75, -0.75));
        }
    }
    // the second round of six adds a circle round the symbol, the third a cross through it
    if k >= 6 && k % 6 != 0 {
        out.push(Draw { rgba, prim: Prim::Circle { c, r: r * 1.25, w, fill: false } });
    }
    if k >= 12 {
        seg(out, at(-1.3, 0.0), at(1.3, 0.0));
    }
}

/// Every hole as (centre, diameter, plated).
fn holes(x: &Prep) -> Vec<(P, f64, bool)> {
    let mut v: Vec<(P, f64, bool)> = x.pads.iter().filter(|p| p.drill > 0.0).map(|p| (p.at, p.drill, true)).collect();
    v.extend(x.p.board.vias.iter().map(|h| (h.at, h.drill, true)));
    v.extend(x.p.board.holes.iter().map(|h| (h.at, h.d, false)));
    v
}

/// The drill drawing: the outline, a symbol per hole size at each hole, and a table of sizes.
pub fn drill_drawing(p: &Pcb) -> Vec<Draw> {
    let x = prep(p);
    let ink = [20, 20, 20, 255];
    let mut out = vec![];
    let o = &x.p.board.outline;
    for i in 0..o.len() {
        out.push(Draw { rgba: ink, prim: Prim::Seg { a: o[i], b: o[(i + 1) % o.len()], w: 0.15 } });
    }
    let hs = holes(&x);
    let mut sizes: Vec<(f64, bool)> = vec![];
    for (_, d, pl) in &hs {
        if !sizes.iter().any(|s| (s.0 - d).abs() < 1e-6 && s.1 == *pl) {
            sizes.push((*d, *pl));
        }
    }
    sizes.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));
    let sym_r = 0.6;
    for (c, d, pl) in &hs {
        let k = sizes.iter().position(|s| (s.0 - d).abs() < 1e-6 && s.1 == *pl).unwrap_or(0);
        drill_symbol(&mut out, k, *c, sym_r, ink);
    }
    // the table under the board
    let (lo, _) = x.p.board.bbox();
    let id = |q: P| q;
    let mut y = lo[1] - 5.0;
    pcb_export::text(&mut out, "Drill   Diameter   Count   Plated", [lo[0] + 3.0, y], 1.6, 0, 0.0, ink, &id);
    for (k, (d, pl)) in sizes.iter().enumerate() {
        y -= 3.2;
        drill_symbol(&mut out, k, [lo[0] + 1.0, y + 0.8], sym_r, ink);
        let n = hs.iter().filter(|h| (h.1 - d).abs() < 1e-6 && h.2 == *pl).count();
        pcb_export::text(&mut out, &format!("{:<7} {:.2} mm    {:<6}  {}", format!("T{}", k + 1), d, n, if *pl { "yes" } else { "no" }), [lo[0] + 3.0, y], 1.6, 0, 0.0, ink, &id);
    }
    y -= 3.2;
    pcb_export::text(&mut out, &format!("{} holes, board {:.2} x {:.2} mm, {} mm thick", hs.len(), x.p.board.size()[0], x.p.board.size()[1], x.p.board.thickness), [lo[0] + 3.0, y], 1.6, 0, 0.0, ink, &id);
    out
}

/// A display list as Gerber strokes and regions.
fn gerber_of(list: &[Draw], function: &str) -> String {
    let mut aps: Vec<String> = vec![];
    let mut body = String::new();
    let mut cur = String::new();
    let xy = |q: P| format!("X{}Y{}", (q[0] * 1e6).round() as i64, (q[1] * 1e6).round() as i64);
    let mut select = |def: String, body: &mut String| {
        if def == cur {
            return;
        }
        let i = match aps.iter().position(|a| *a == def) {
            Some(i) => i,
            None => {
                aps.push(def.clone());
                aps.len() - 1
            }
        };
        let _ = writeln!(body, "D{}*", i + 10);
        cur = def;
    };
    for d in list {
        match &d.prim {
            Prim::Seg { a, b, w } => {
                select(format!("C,{:.4}", w.max(0.05)), &mut body);
                let _ = writeln!(body, "{}D02*\n{}D01*", xy(*a), xy(*b));
            }
            Prim::Circle { c, r, w, fill } => {
                if *fill {
                    select(format!("C,{:.4}", 2.0 * r), &mut body);
                    let _ = writeln!(body, "{}D03*", xy(*c));
                } else {
                    select(format!("C,{:.4}", w.max(0.05)), &mut body);
                    let n = 32;
                    for i in 0..=n {
                        let t = std::f64::consts::TAU * i as f64 / n as f64;
                        let q = [c[0] + r * t.cos(), c[1] + r * t.sin()];
                        let _ = writeln!(body, "{}{}*", xy(q), if i == 0 { "D02" } else { "D01" });
                    }
                }
            }
            Prim::Fill(cs) => {
                for c in cs.iter().filter(|c| c.len() >= 3) {
                    body += "G36*\n";
                    let _ = writeln!(body, "{}D02*", xy(c[0]));
                    for q in &c[1..] {
                        let _ = writeln!(body, "{}D01*", xy(*q));
                    }
                    let _ = writeln!(body, "{}D01*\nG37*", xy(c[0]));
                }
            }
        }
    }
    let mut s = format!("G04 Made by rCAD*\n%TF.GenerationSoftware,rCAD,PCB*%\n%TF.FileFunction,{function}*%\n%TF.FilePolarity,Positive*%\n%FSLAX46Y46*%\n%MOMM*%\n");
    for (i, a) in aps.iter().enumerate() {
        let _ = writeln!(s, "%ADD{}{}*%", i + 10, a);
    }
    s += "%LPD*%\nG01*\n";
    s += &body;
    s += "M02*\n";
    s
}

// ---------------------------------------------------------------- PDF

/// The assembly drawing of one side: outline, pads, silkscreen and courtyards, references
/// (the bottom seen from below, mirrored).
pub fn assembly(p: &Pcb, side: Layer) -> Vec<Draw> {
    let x = prep(p);
    let mirror = side == Layer::Bottom;
    let (lo, hi) = x.p.board.bbox();
    let mx = |q: P| if mirror { [lo[0] + hi[0] - q[0], q[1]] } else { q };
    let mut out = vec![];
    let o = &x.p.board.outline;
    for i in 0..o.len() {
        out.push(Draw { rgba: [20, 20, 20, 255], prim: Prim::Seg { a: mx(o[i]), b: mx(o[(i + 1) % o.len()]), w: 0.2 } });
    }
    for pad in x.pads.iter().filter(|q| q.on(side)) {
        let c: Vec<P> = pcb_export::pad_outline(pad, 0.0).into_iter().map(mx).collect();
        out.push(Draw { rgba: [200, 200, 200, 255], prim: Prim::Fill(vec![c]) });
    }
    for part in x.p.parts.iter().filter(|q| q.side() == side) {
        let mut list = vec![];
        let fp = part.footprint();
        let (a, b) = fp.court;
        let court = [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]].map(|q| mx(part.xf(q)));
        for i in 0..4 {
            list.push(Draw { rgba: [120, 120, 120, 255], prim: Prim::Seg { a: court[i], b: court[(i + 1) % 4], w: 0.08 } });
        }
        for g in &fp.silk {
            let mut l2 = vec![];
            crate::pcb_export::silk(&mut l2, part, [30, 30, 30, 255]);
            let _ = g;
            list.extend(l2.into_iter().map(|d| match d.prim {
                Prim::Seg { a, b, w } => Draw { rgba: d.rgba, prim: Prim::Seg { a: mx(a), b: mx(b), w } },
                o => Draw { rgba: d.rgba, prim: o },
            }));
            break;
        }
        // the reference in the middle of the part, upright
        let c = mx(part.at);
        let h = ((b[1] - a[1]).min(b[0] - a[0]) * 0.35).clamp(0.6, 1.6);
        pcb_export::text(&mut list, &part.reference, [c[0], c[1] - h / 2.0], h, 1, 0.0, [0, 70, 160, 255], &|q| q);
        out.extend(list);
    }
    let id = |q: P| q;
    let label = format!("{} - assembly, {} side{}", x.p.name, if mirror { "bottom" } else { "top" }, if mirror { " (seen from below)" } else { "" });
    pcb_export::text(&mut out, &label, [lo[0], lo[1] - 4.0], 1.8, 0, 0.0, [20, 20, 20, 255], &id);
    out
}

fn pdf_page(list: &[Draw], page: [f64; 2]) -> String {
    let k = 72.0 / 25.4;
    let (lo, hi) = pcb_export::extent(list);
    let m = 12.0;
    let s = ((page[0] - 2.0 * m) / (hi[0] - lo[0]).max(1.0)).min((page[1] - 2.0 * m) / (hi[1] - lo[1]).max(1.0)).min(8.0);
    let ox = (page[0] - (hi[0] - lo[0]) * s) / 2.0;
    let oy = (page[1] - (hi[1] - lo[1]) * s) / 2.0;
    let t = |q: P| [(ox + (q[0] - lo[0]) * s) * k, (oy + (q[1] - lo[1]) * s) * k];
    let col = |c: [u8; 4]| format!("{:.3} {:.3} {:.3}", c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0);
    let mut c = String::from("1 J 1 j\n");
    for d in list {
        // white and very light ink would vanish on paper, except fills meant as pads
        match &d.prim {
            Prim::Seg { a, b, w } => {
                let (a, b) = (t(*a), t(*b));
                let _ = writeln!(c, "{} RG {:.3} w {:.2} {:.2} m {:.2} {:.2} l S", col(d.rgba), (w * s * k).max(0.25), a[0], a[1], b[0], b[1]);
            }
            Prim::Circle { c: ctr, r, w, fill } => {
                let n = 36;
                let pts: Vec<P> = (0..n).map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / n as f64;
                    t([ctr[0] + r * a.cos(), ctr[1] + r * a.sin()])
                }).collect();
                let _ = write!(c, "{} {} {:.3} w ", col(d.rgba), if *fill { "rg" } else { "RG" }, (w * s * k).max(0.25));
                for (i, q) in pts.iter().enumerate() {
                    let _ = write!(c, "{:.2} {:.2} {} ", q[0], q[1], if i == 0 { "m" } else { "l" });
                }
                c += if *fill { "h f\n" } else { "h S\n" };
            }
            Prim::Fill(cs) => {
                let _ = write!(c, "{} rg ", col(d.rgba));
                for poly in cs.iter().filter(|p| p.len() >= 3) {
                    for (i, q) in poly.iter().enumerate() {
                        let q = t(*q);
                        let _ = write!(c, "{:.2} {:.2} {} ", q[0], q[1], if i == 0 { "m" } else { "l" });
                    }
                    c += "h ";
                }
                c += "f*\n";
            }
        }
    }
    c
}

/// A PDF with one page per display list (A4 landscape, each fitted to the page).
pub fn pdf(pages: &[Vec<Draw>]) -> Vec<u8> {
    let size = [297.0, 210.0];
    let k = 72.0 / 25.4;
    let contents: Vec<String> = pages.iter().map(|l| pdf_page(l, size)).collect();
    let n = contents.len().max(1);
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
    let mut objs = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" "))];
    for (i, c) in contents.iter().enumerate() {
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Contents {} 0 R >>", size[0] * k, size[1] * k, 4 + 2 * i));
        objs.push(format!("<< /Length {} >>\nstream\n{}endstream", c.len(), c));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{}\nendobj\n", i + 1, o).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", objs.len() + 1, xref).as_bytes());
    out
}

/// The print set: schematic, top assembly, bottom assembly (when parts are there), drill drawing.
pub fn prints(p: &Pcb) -> Vec<u8> {
    let nl = p.netlist();
    let mut pages = vec![pcb_export::schematic_scene(p, &nl, None), assembly(p, Layer::Top)];
    if p.parts.iter().any(|q| q.bottom) {
        pages.push(assembly(p, Layer::Bottom));
    }
    pages.push(drill_drawing(p));
    pdf(&pages)
}

// ---------------------------------------------------------------- ODB++

/// ODB++ symbol names (sizes in micrometres) for a pad, or None when it must be a surface.
fn odb_symbol(pad: &WPad) -> Option<String> {
    let um = |v: f64| (v * 1000.0).round() as i64;
    let q = pad.rot.rem_euclid(90.0);
    if q > 1e-6 && (90.0 - q) > 1e-6 && pad.shape != crate::pcb_lib::PadShape::Round {
        return None;
    }
    let swap = ((pad.rot / 90.0).round() as i64).rem_euclid(2) == 1;
    let (w, h) = if swap { (pad.size[1], pad.size[0]) } else { (pad.size[0], pad.size[1]) };
    Some(match pad.shape {
        crate::pcb_lib::PadShape::Round => format!("r{}", um(w.min(h))),
        crate::pcb_lib::PadShape::Oval => format!("oval{}x{}", um(w), um(h)),
        _ => format!("rect{}x{}", um(w), um(h)),
    })
}

struct Features {
    syms: Vec<String>,
    body: String,
}

impl Features {
    fn new() -> Features {
        Features { syms: vec![], body: String::new() }
    }
    fn sym(&mut self, s: String) -> usize {
        match self.syms.iter().position(|x| *x == s) {
            Some(i) => i,
            None => {
                self.syms.push(s);
                self.syms.len() - 1
            }
        }
    }
    fn line(&mut self, a: P, b: P, w: f64) {
        let i = self.sym(format!("r{}", (w.max(0.01) * 1000.0).round() as i64));
        let _ = writeln!(self.body, "L {:.4} {:.4} {:.4} {:.4} {i} P 0", a[0], a[1], b[0], b[1]);
    }
    fn pad_sym(&mut self, at: P, sym: String) {
        let i = self.sym(sym);
        let _ = writeln!(self.body, "P {:.4} {:.4} {i} P 0 0", at[0], at[1]);
    }
    fn surface(&mut self, shapes: &[Vec<P>]) {
        if shapes.is_empty() || shapes[0].len() < 3 {
            return;
        }
        self.body += "S P 0\n";
        for (k, c) in shapes.iter().enumerate() {
            if c.len() < 3 {
                continue;
            }
            // islands clockwise, holes anticlockwise
            let area: f64 = (0..c.len()).map(|i| c[i][0] * c[(i + 1) % c.len()][1] - c[(i + 1) % c.len()][0] * c[i][1]).sum();
            let want_cw = k == 0;
            let pts: Vec<P> = if (area < 0.0) == want_cw { c.clone() } else { c.iter().rev().cloned().collect() };
            let _ = writeln!(self.body, "OB {:.4} {:.4} {}", pts[0][0], pts[0][1], if k == 0 { "I" } else { "H" });
            for q in &pts[1..] {
                let _ = writeln!(self.body, "OS {:.4} {:.4}", q[0], q[1]);
            }
            let _ = writeln!(self.body, "OS {:.4} {:.4}", pts[0][0], pts[0][1]);
            self.body += "OE\n";
        }
        self.body += "SE\n";
    }
    fn pad(&mut self, pad: &WPad, grow: f64) {
        let mut g = pad.clone();
        g.size = [pad.size[0] + 2.0 * grow, pad.size[1] + 2.0 * grow];
        match odb_symbol(&g) {
            Some(s) => self.pad_sym(g.at, s),
            None => self.surface(&[pcb_export::pad_outline(pad, grow)]),
        }
    }
    fn text(self) -> String {
        let mut s = String::from("UNITS=MM\n#\n#Feature symbol names\n#\n");
        for (i, x) in self.syms.iter().enumerate() {
            let _ = writeln!(s, "${i} {x}");
        }
        s += "#\n#Layer features\n#\n";
        s += &self.body;
        s
    }
}

/// An ODB++ (v7) job as files under `odb/<job>/`: matrix, profile, copper, mask, paste,
/// silkscreen and drill layers, components and the netlist.
pub fn odb(p: &Pcb) -> Vec<(String, Vec<u8>)> {
    let x = prep(p);
    let fills = pcb::fill_zones(&x.p, &x.nl, &x.pads);
    let job = base_name(&x.p).to_lowercase();
    let root = format!("odb/{job}");
    let step = format!("{root}/steps/pcb");
    let mut files: Vec<(String, String)> = vec![];
    // layers top to bottom: (name, type, polarity)
    let layers = [
        ("comp_+_top", "COMPONENT"),
        ("top_silk", "SILK_SCREEN"),
        ("top_paste", "SOLDER_PASTE"),
        ("top_mask", "SOLDER_MASK"),
        ("top", "SIGNAL"),
        ("bottom", "SIGNAL"),
        ("bottom_mask", "SOLDER_MASK"),
        ("bottom_paste", "SOLDER_PASTE"),
        ("bottom_silk", "SILK_SCREEN"),
        ("comp_+_bot", "COMPONENT"),
        ("drill", "DRILL"),
    ];
    let mut matrix = String::from("STEP {\n    COL=1\n    NAME=PCB\n}\n\n");
    for (i, (name, ty)) in layers.iter().enumerate() {
        let (start, end) = if *ty == "DRILL" { ("top", "bottom") } else { ("", "") };
        let _ = write!(matrix, "LAYER {{\n    ROW={}\n    CONTEXT=BOARD\n    TYPE={ty}\n    NAME={}\n    POLARITY=POSITIVE\n    START_NAME={}\n    END_NAME={}\n    OLD_NAME=\n    COLOR=0\n}}\n\n", i + 1, name.to_uppercase(), start.to_uppercase(), end.to_uppercase());
    }
    files.push((format!("{root}/matrix/matrix"), matrix));
    files.push((format!("{root}/misc/info"), format!("JOB_NAME={job}\nODB_VERSION_MAJOR=7\nODB_VERSION_MINOR=0\nODB_SOURCE=rCAD\nCREATION_DATE=\nSAVE_APP=rCAD {}\nUNITS=MM\n", env!("CARGO_PKG_VERSION"))));
    files.push((format!("{step}/stephdr"), "UNITS=MM\nX_DATUM=0\nY_DATUM=0\nX_ORIGIN=0\nY_ORIGIN=0\n".into()));
    let mut prof = Features::new();
    let mut outline = vec![pcb::ccw(x.p.board.outline.clone())];
    outline.extend(x.p.board.holes.iter().map(|h| pcb::capsule(h.at, h.at, h.d / 2.0)));
    prof.surface(&outline);
    files.push((format!("{step}/profile"), prof.text()));
    for side in [Layer::Top, Layer::Bottom] {
        let tag = if side == Layer::Top { "top" } else { "bottom" };
        let mut cu = Features::new();
        for (zi, _) in x.p.board.zones.iter().enumerate().filter(|(_, z)| z.layer == side) {
            for piece in fills.get(zi).into_iter().flatten() {
                cu.surface(piece);
            }
        }
        for t in x.p.board.tracks.iter().filter(|t| t.layer == side) {
            cu.line(t.a, t.b, t.width);
        }
        for pad in x.pads.iter().filter(|q| q.on(side)) {
            cu.pad(pad, 0.0);
        }
        for v in &x.p.board.vias {
            cu.pad_sym(v.at, format!("r{}", (v.d * 1000.0).round() as i64));
        }
        files.push((format!("{step}/layers/{tag}/features"), cu.text()));
        let mut mask = Features::new();
        let mut paste = Features::new();
        for pad in x.pads.iter().filter(|q| q.on(side)) {
            mask.pad(pad, 0.05);
            if pad.drill <= 0.0 {
                paste.pad(pad, 0.0);
            }
        }
        files.push((format!("{step}/layers/{tag}_mask/features"), mask.text()));
        files.push((format!("{step}/layers/{tag}_paste/features"), paste.text()));
        let mut silk = Features::new();
        let mut list = vec![];
        for part in x.p.parts.iter().filter(|q| q.side() == side) {
            pcb_export::silk(&mut list, part, [0; 4]);
        }
        for d in &list {
            match &d.prim {
                Prim::Seg { a, b, w } => silk.line(*a, *b, w.max(0.12)),
                Prim::Circle { c, r, w, .. } => {
                    let n = 24;
                    for i in 0..n {
                        let t0 = std::f64::consts::TAU * i as f64 / n as f64;
                        let t1 = std::f64::consts::TAU * (i + 1) as f64 / n as f64;
                        silk.line([c[0] + r * t0.cos(), c[1] + r * t0.sin()], [c[0] + r * t1.cos(), c[1] + r * t1.sin()], w.max(0.12));
                    }
                }
                Prim::Fill(cs) => silk.surface(cs),
            }
        }
        files.push((format!("{step}/layers/{tag}_silk/features"), silk.text()));
    }
    let mut drill = Features::new();
    for (c, d, _) in holes(&x) {
        drill.pad_sym(c, format!("r{}", (d * 1000.0).round() as i64));
    }
    files.push((format!("{step}/layers/drill/features"), drill.text()));
    // components and their pins, per side
    for (side, dir) in [(Layer::Top, "comp_+_top"), (Layer::Bottom, "comp_+_bot")] {
        let mut s = String::from("UNITS=MM\n#\n");
        for (ci, (pi, part)) in x.p.parts.iter().enumerate().filter(|(_, q)| q.side() == side).enumerate() {
            let _ = writeln!(s, "# CMP {ci}");
            let _ = writeln!(s, "CMP 0 {:.4} {:.4} {:.3} {} {} {} ;", part.at[0], part.at[1], part.rot.rem_euclid(360.0), if part.bottom { "M" } else { "N" }, part.reference, part.footprint);
            let _ = writeln!(s, "PRP VALUE '{}'", part.value.replace('\'', ""));
            for (k, pad) in x.pads.iter().filter(|q| q.part == pi).enumerate() {
                let net = pad.net.map(|n| n as i64).unwrap_or(-1);
                let _ = writeln!(s, "TOP {k} {:.4} {:.4} {:.3} N {net} 0 {}", pad.at[0], pad.at[1], pad.rot.rem_euclid(360.0), pad.num);
            }
            s += "#\n";
        }
        files.push((format!("{step}/layers/{dir}/components"), s));
    }
    let mut eda = String::from("HDR rCAD\nUNITS=MM\n#\n");
    for (ni, n) in x.nl.nets.iter().enumerate() {
        let _ = writeln!(eda, "# NET {ni}");
        let _ = writeln!(eda, "NET {}", n.name.replace(' ', "_"));
        for (pi, num) in &n.pins {
            if let Some(pad) = x.pads.iter().find(|q| q.part == *pi && &q.num == num) {
                let side = if pad.layer == Some(Layer::Bottom) { "B" } else { "T" };
                let _ = writeln!(eda, "SNT TOP {side} {} {}", pi, num);
            }
        }
    }
    files.push((format!("{step}/eda/data"), eda));
    files.into_iter().map(|(n, s)| (n, s.into_bytes())).collect()
}

// ---------------------------------------------------------------- the job

/// Every file the job makes, by name.
pub fn outputs(p: &Pcb, job: &OutJob) -> Vec<(String, Vec<u8>)> {
    let base = base_name(p);
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    if job.gerbers {
        let g = pcb_export::gerbers(p);
        files.push((format!("gerbers/{base}-job.gbrjob"), gbrjob(p, &g).into_bytes()));
        files.extend(g.into_iter().map(|(n, s)| (format!("gerbers/{n}"), s.into_bytes())));
    }
    if job.drill_map {
        files.push((format!("gerbers/{base}-Drill_Map.gbr"), gerber_of(&drill_drawing(p), "Drillmap").into_bytes()));
    }
    if job.bom {
        files.push((format!("assembly/{base}-BOM.csv"), pcb_export::bom_csv(p).into_bytes()));
    }
    if job.cpl {
        files.push((format!("assembly/{base}-CPL.csv"), pcb_export::cpl_csv(p).into_bytes()));
    }
    if job.ipc356 {
        files.push((format!("test/{base}.ipc"), ipc356(p).into_bytes()));
    }
    if job.pdf {
        files.push((format!("{base}-prints.pdf"), prints(p)));
    }
    if job.odb {
        files.extend(odb(p));
    }
    files
}

/// Run the job: a path ending .zip gets one ZIP of everything, any other path is a folder.
pub fn run(p: &Pcb, job: &OutJob, path: &std::path::Path) -> Result<Vec<String>, String> {
    let files = outputs(p, job);
    if files.is_empty() {
        return Err("the output job has nothing turned on".into());
    }
    let write = |q: &std::path::Path, data: &[u8]| -> Result<(), String> {
        if let Some(dir) = q.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(q, data).map_err(|e| format!("{}: {e}", q.display()))
    };
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        write(path, &pcb_export::zip(&files))?;
        Ok(vec![path.display().to_string()])
    } else {
        let mut out = vec![];
        for (n, data) in &files {
            let q = path.join(n);
            write(&q, data)?;
            out.push(q.display().to_string());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Pcb {
        let mut p = crate::pcb::tests::led_board();
        p.board.holes.push(pcb::Hole { at: [3.0, 3.0], d: 3.2 });
        crate::pcb_route::autoroute(&mut p, None);
        p
    }

    #[test]
    fn ipc356_lists_every_pad_and_via() {
        let p = board();
        let s = ipc356(&p);
        let nl = p.netlist();
        let pads = p.pads(&nl).len();
        let rows = s.lines().filter(|l| l.starts_with("317") || l.starts_with("327")).count();
        assert_eq!(rows, pads + p.board.vias.len(), "{s}");
        assert!(s.lines().any(|l| l.starts_with("317GND") && l.contains("J1    -2")), "{s}");
        assert!(s.lines().any(|l| l.starts_with("327VCC") && l.contains("R1    -1")), "{s}");
        assert!(s.ends_with("999\n"));
        // fixed columns: reference from column 21, access at 39, X at 42, Y at 50, rotation at 68
        for r in s.lines().filter(|l| l.starts_with("317") || l.starts_with("327")) {
            assert_eq!((&r[20..21] != " ", &r[38..39], &r[41..42], &r[49..50], &r[67..68]), (true, "A", "X", "Y", "R"), "{r}");
        }
    }

    #[test]
    fn the_job_writes_everything() {
        let p = board();
        let files = outputs(&p, &OutJob::default());
        let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
        for want in ["gerbers/LED-job.gbrjob", "gerbers/LED-F_Cu.gtl", "gerbers/LED-PTH.drl", "gerbers/LED-Drill_Map.gbr", "assembly/LED-BOM.csv", "assembly/LED-CPL.csv", "test/LED.ipc", "LED-prints.pdf", "odb/led/matrix/matrix", "odb/led/steps/pcb/layers/top/features", "odb/led/steps/pcb/layers/drill/features", "odb/led/steps/pcb/eda/data", "odb/led/steps/pcb/profile"] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
        let job: serde_json::Value = serde_json::from_slice(&files.iter().find(|f| f.0.ends_with(".gbrjob")).unwrap().1).unwrap();
        assert!(job["FilesAttributes"].as_array().unwrap().iter().any(|l| l["FileFunction"] == "Copper,L1,Top"));
        let pdf = &files.iter().find(|f| f.0.ends_with(".pdf")).unwrap().1;
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
        assert_eq!(String::from_utf8_lossy(pdf).matches("/Type /Page ").count(), 3);
        let top = String::from_utf8(files.iter().find(|f| f.0.ends_with("layers/top/features")).unwrap().1.clone()).unwrap();
        assert!(top.starts_with("UNITS=MM") && top.contains("\nL ") && top.contains("\nP "), "{top}");
        let off = OutJob { odb: false, pdf: false, ..Default::default() };
        assert!(!outputs(&p, &off).iter().any(|f| f.0.starts_with("odb/") || f.0.ends_with(".pdf")));
    }
}
