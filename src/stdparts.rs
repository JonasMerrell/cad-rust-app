//! Standard parts: ISO screws, nuts, washers, pins, bearings, motors, rods and extrusions,
//! like Onshape's Standard Content. Each standard is a custom feature (a Rhai script holding
//! its dimension table) in a "Standard parts" Part Studio, so a part stays parametric: edit
//! the feature's size or length and every instance of it follows. Threads are not modelled.
//!
//! Placement conventions (the part's own coordinates):
//! - screws: the seating plane (under the head, or the top of a countersunk or set screw) is
//!   z = 0, the shank points down -Z;
//! - nuts, washers, bearings, pins, rods and extrusions stand on z = 0 along +Z;
//! - motors: the mounting face is z = 0, the body below and the shaft pointing up.

use crate::doc::{BodyId, BoolOp, Document, Op, PartProps};
use crate::script::{CustomDef, ParamDef};

pub const STUDIO: &str = "Standard parts";

pub struct Standard {
    /// Short id used by the agent tools and in feature names, e.g. "ISO 4762".
    pub key: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    /// Size label from the size number: "M{}", "{}", "NEMA {}".
    pub size_fmt: &'static str,
    /// Part name: {k} key, {s} size label, {l} length.
    pub title: &'static str,
    /// Dimension table: first column is the size; with a length, the last two are the
    /// shortest and longest catalogue lengths (not passed to the script).
    pub rows: &'static [&'static [f64]],
    /// Catalogue lengths (filtered by each row's range); None: the part has no length.
    pub lengths: Option<&'static [f64]>,
    pub default: (f64, f64),
    pub material: &'static str,
    pub color: [u8; 3],
    /// Script body; `row` holds the size's table row, `size` and `length` the values.
    pub script: &'static str,
}

const SCREW_LENGTHS: &[f64] =
    &[3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0];
const PIN_LENGTHS: &[f64] = &[4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 60.0, 70.0, 80.0, 100.0];
const ROD_LENGTHS: &[f64] = &[50.0, 75.0, 100.0, 150.0, 200.0, 250.0, 300.0, 400.0, 500.0, 600.0, 800.0, 1000.0];
const MOTOR_LENGTHS: &[f64] = &[20.0, 25.0, 28.0, 34.0, 40.0, 48.0, 56.0, 60.0, 76.0, 100.0];

const BLACK_OXIDE: [u8; 3] = [40, 42, 46];
const ZINC: [u8; 3] = [188, 191, 196];
const BRIGHT: [u8; 3] = [205, 208, 213];

/// Helpers every script gets: `hex(s)` (hexagon across flats `s`), `hexc(s, h, top, bottom)`
/// (a hex prism with 30° chamfers) and `rp(x0, y0, x1, y1, h)` (a rectangle prism).
const HELPERS: &str = r#"
fn hex(s) {
    let r = s / sqrt(3.0);
    let p = [];
    for i in 0..6 { let a = (30.0 + 60.0 * i).to_radians(); p.push([r * cos(a), r * sin(a)]); }
    p
}
fn hexc(s, h, top, bottom) {
    let r0 = 0.475 * s;
    let big = 1.02 * s / sqrt(3.0);
    let dz = (big - r0) * 0.57735;
    let p = [[0.0, 0.0]];
    if bottom { p.push([r0, 0.0]); p.push([big, dz]); } else { p.push([big, 0.0]); }
    if top { p.push([big, h - dz]); p.push([r0, h]); } else { p.push([big, h]); }
    p.push([0.0, h]);
    prism(hex(s), h) & revolve(p, 360.0)
}
fn rp(x0, y0, x1, y1, h) { prism([[x0, y0], [x1, y0], [x1, y1], [x0, y1]], h) }
"#;

pub static STANDARDS: &[Standard] = &[
    Standard {
        key: "ISO 4762",
        name: "Socket head cap screw",
        category: "Screws",
        size_fmt: "M{}",
        title: "{k} {s}x{l}",
        // d, head dia dk, head height k, hex key s, socket depth t, lengths
        rows: &[
            &[2.0, 3.8, 2.0, 1.5, 1.0, 3.0, 20.0],
            &[2.5, 4.5, 2.5, 2.0, 1.1, 4.0, 25.0],
            &[3.0, 5.5, 3.0, 2.5, 1.3, 4.0, 40.0],
            &[4.0, 7.0, 4.0, 3.0, 2.0, 5.0, 50.0],
            &[5.0, 8.5, 5.0, 4.0, 2.5, 6.0, 60.0],
            &[6.0, 10.0, 6.0, 5.0, 3.0, 8.0, 80.0],
            &[8.0, 13.0, 8.0, 6.0, 4.0, 10.0, 100.0],
            &[10.0, 16.0, 10.0, 8.0, 5.0, 12.0, 120.0],
            &[12.0, 18.0, 12.0, 10.0, 6.0, 16.0, 120.0],
            &[16.0, 24.0, 16.0, 14.0, 8.0, 20.0, 120.0],
            &[20.0, 30.0, 20.0, 17.0, 10.0, 30.0, 120.0],
        ],
        lengths: Some(SCREW_LENGTHS),
        default: (5.0, 16.0),
        material: "Steel",
        color: BLACK_OXIDE,
        script: r#"
let d = size; let dk = row[1]; let k = row[2]; let s = row[3]; let t = row[4];
let c = 0.08 * d; let kc = 0.06 * dk;
let body = revolve([[0.0, -length], [d / 2 - c, -length], [d / 2, -length + c], [d / 2, 0.0], [dk / 2, 0.0], [dk / 2, k - kc], [dk / 2 - kc, k], [0.0, k]], 360.0);
body - prism(hex(s), t + 1.0).move(0.0, 0.0, k - t)
"#,
    },
    Standard {
        key: "ISO 7380",
        name: "Button head screw",
        category: "Screws",
        size_fmt: "M{}",
        title: "{k} {s}x{l}",
        // d, dk, k, s, t, lengths
        rows: &[
            &[3.0, 5.7, 1.65, 2.0, 1.04, 4.0, 30.0],
            &[4.0, 7.6, 2.2, 2.5, 1.3, 5.0, 40.0],
            &[5.0, 9.5, 2.75, 3.0, 1.56, 6.0, 40.0],
            &[6.0, 10.5, 3.3, 4.0, 2.08, 8.0, 40.0],
            &[8.0, 14.0, 4.4, 5.0, 2.6, 10.0, 50.0],
            &[10.0, 17.5, 5.5, 6.0, 3.12, 12.0, 50.0],
            &[12.0, 21.0, 6.6, 8.0, 4.16, 16.0, 50.0],
        ],
        lengths: Some(SCREW_LENGTHS),
        default: (4.0, 10.0),
        material: "Steel",
        color: BLACK_OXIDE,
        script: r#"
let d = size; let dk = row[1]; let k = row[2]; let s = row[3]; let t = row[4];
let c = 0.08 * d;
let p = [[0.0, -length], [d / 2 - c, -length], [d / 2, -length + c], [d / 2, 0.0], [dk / 2, 0.0]];
for i in 0..8 { let a = (90.0 * i / 8.0).to_radians(); p.push([dk / 2 * cos(a), 0.15 * k + 0.85 * k * sin(a)]); }
p.push([0.0, k]);
revolve(p, 360.0) - prism(hex(s), t + 1.0).move(0.0, 0.0, k - t)
"#,
    },
    Standard {
        key: "ISO 10642",
        name: "Countersunk socket screw",
        category: "Screws",
        size_fmt: "M{}",
        title: "{k} {s}x{l}",
        // d, dk, s, t, lengths (the length includes the head)
        rows: &[
            &[3.0, 6.72, 2.0, 1.1, 6.0, 30.0],
            &[4.0, 8.96, 2.5, 1.5, 8.0, 40.0],
            &[5.0, 11.2, 3.0, 1.9, 8.0, 50.0],
            &[6.0, 13.44, 4.0, 2.2, 8.0, 60.0],
            &[8.0, 17.92, 5.0, 3.0, 10.0, 80.0],
            &[10.0, 22.4, 6.0, 3.6, 12.0, 100.0],
            &[12.0, 26.88, 8.0, 4.3, 20.0, 100.0],
        ],
        lengths: Some(SCREW_LENGTHS),
        default: (4.0, 12.0),
        material: "Steel",
        color: BLACK_OXIDE,
        script: r#"
let d = size; let dk = row[1]; let s = row[2]; let t = row[3];
let k = (dk - d) / 2; let c = 0.08 * d;
if length <= k + c { throw "the length (with the head) must be over " + (k + c) + " mm"; }
let body = revolve([[0.0, 0.0], [dk / 2, 0.0], [d / 2, -k], [d / 2, -length + c], [d / 2 - c, -length], [0.0, -length]], 360.0);
body - prism(hex(s), t + 1.0).move(0.0, 0.0, -t)
"#,
    },
    Standard {
        key: "ISO 4017",
        name: "Hex head screw",
        category: "Screws",
        size_fmt: "M{}",
        title: "{k} {s}x{l}",
        // d, across flats s, head height k, lengths
        rows: &[
            &[3.0, 5.5, 2.0, 5.0, 30.0],
            &[4.0, 7.0, 2.8, 6.0, 40.0],
            &[5.0, 8.0, 3.5, 8.0, 50.0],
            &[6.0, 10.0, 4.0, 10.0, 60.0],
            &[8.0, 13.0, 5.3, 12.0, 80.0],
            &[10.0, 16.0, 6.4, 16.0, 100.0],
            &[12.0, 18.0, 7.5, 20.0, 120.0],
            &[16.0, 24.0, 10.0, 30.0, 120.0],
            &[20.0, 30.0, 12.5, 40.0, 120.0],
        ],
        lengths: Some(SCREW_LENGTHS),
        default: (6.0, 20.0),
        material: "Steel",
        color: ZINC,
        script: r#"
let d = size; let s = row[1]; let k = row[2];
let c = 0.08 * d;
let shank = revolve([[0.0, -length], [d / 2 - c, -length], [d / 2, -length + c], [d / 2, k / 2], [0.0, k / 2]], 360.0);
hexc(s, k, true, false) + shank
"#,
    },
    Standard {
        key: "ISO 4026",
        name: "Set screw, flat point",
        category: "Screws",
        size_fmt: "M{}",
        title: "{k} {s}x{l}",
        // d, hex key s, key depth t, lengths
        rows: &[
            &[3.0, 1.5, 1.5, 3.0, 16.0],
            &[4.0, 2.0, 2.0, 4.0, 20.0],
            &[5.0, 2.5, 2.5, 5.0, 25.0],
            &[6.0, 3.0, 3.0, 6.0, 30.0],
            &[8.0, 4.0, 4.0, 8.0, 40.0],
            &[10.0, 5.0, 5.0, 10.0, 50.0],
            &[12.0, 6.0, 6.0, 12.0, 60.0],
        ],
        lengths: Some(SCREW_LENGTHS),
        default: (4.0, 6.0),
        material: "Steel",
        color: BLACK_OXIDE,
        script: r#"
let d = size; let s = row[1]; let t = row[2];
if t > 0.6 * length { t = 0.6 * length; }
let c = 0.1 * d;
let body = revolve([[0.0, 0.0], [d / 2 - c, 0.0], [d / 2, -c], [d / 2, -length + c], [d / 2 - c, -length], [0.0, -length]], 360.0);
body - prism(hex(s), t + 1.0).move(0.0, 0.0, -t)
"#,
    },
    Standard {
        key: "ISO 4032",
        name: "Hex nut",
        category: "Nuts",
        size_fmt: "M{}",
        title: "{k} {s}",
        // d, across flats s, height m
        rows: &[
            &[2.0, 4.0, 1.6],
            &[2.5, 5.0, 2.0],
            &[3.0, 5.5, 2.4],
            &[4.0, 7.0, 3.2],
            &[5.0, 8.0, 4.7],
            &[6.0, 10.0, 5.2],
            &[8.0, 13.0, 6.8],
            &[10.0, 16.0, 8.4],
            &[12.0, 18.0, 10.8],
            &[16.0, 24.0, 14.8],
            &[20.0, 30.0, 18.0],
        ],
        lengths: None,
        default: (5.0, 0.0),
        material: "Steel",
        color: ZINC,
        script: r#"
let s = row[1]; let m = row[2];
hexc(s, m, true, true) - cylinder(size / 2, m + 2.0).move(0.0, 0.0, -1.0)
"#,
    },
    Standard {
        key: "ISO 4035",
        name: "Thin hex nut",
        category: "Nuts",
        size_fmt: "M{}",
        title: "{k} {s}",
        rows: &[
            &[3.0, 5.5, 1.8],
            &[4.0, 7.0, 2.2],
            &[5.0, 8.0, 2.7],
            &[6.0, 10.0, 3.2],
            &[8.0, 13.0, 4.0],
            &[10.0, 16.0, 5.0],
            &[12.0, 18.0, 6.0],
            &[16.0, 24.0, 8.0],
            &[20.0, 30.0, 10.0],
        ],
        lengths: None,
        default: (5.0, 0.0),
        material: "Steel",
        color: ZINC,
        script: r#"
let s = row[1]; let m = row[2];
hexc(s, m, true, true) - cylinder(size / 2, m + 2.0).move(0.0, 0.0, -1.0)
"#,
    },
    Standard {
        key: "DIN 985",
        name: "Nylon lock nut",
        category: "Nuts",
        size_fmt: "M{}",
        title: "{k} {s}",
        // d, across flats s, height h
        rows: &[&[3.0, 5.5, 4.0], &[4.0, 7.0, 5.0], &[5.0, 8.0, 5.0], &[6.0, 10.0, 6.0], &[8.0, 13.0, 8.0], &[10.0, 16.0, 10.0], &[12.0, 18.0, 12.0]],
        lengths: None,
        default: (5.0, 0.0),
        material: "Steel",
        color: ZINC,
        script: r#"
let s = row[1]; let h = row[2];
let collar = revolve([[0.0, 0.5 * h], [0.45 * s, 0.5 * h], [0.45 * s, h - 0.06 * s], [0.39 * s, h], [0.0, h]], 360.0);
(hexc(s, 0.75 * h, false, true) + collar) - cylinder(size / 2, h + 2.0).move(0.0, 0.0, -1.0)
"#,
    },
    Standard {
        key: "ISO 7089",
        name: "Plain washer",
        category: "Washers",
        size_fmt: "M{}",
        title: "{k} {s}",
        // d, bore d1, outside d2, thickness h
        rows: &[
            &[2.0, 2.2, 5.0, 0.3],
            &[2.5, 2.7, 6.0, 0.5],
            &[3.0, 3.2, 7.0, 0.5],
            &[4.0, 4.3, 9.0, 0.8],
            &[5.0, 5.3, 10.0, 1.0],
            &[6.0, 6.4, 12.0, 1.6],
            &[8.0, 8.4, 16.0, 1.6],
            &[10.0, 10.5, 20.0, 2.0],
            &[12.0, 13.0, 24.0, 2.5],
            &[16.0, 17.0, 30.0, 3.0],
            &[20.0, 21.0, 37.0, 3.0],
        ],
        lengths: None,
        default: (5.0, 0.0),
        material: "Steel",
        color: ZINC,
        script: r#"
revolve([[row[1] / 2, 0.0], [row[2] / 2, 0.0], [row[2] / 2, row[3]], [row[1] / 2, row[3]]], 360.0)
"#,
    },
    Standard {
        key: "ISO 7093",
        name: "Large washer",
        category: "Washers",
        size_fmt: "M{}",
        title: "{k} {s}",
        rows: &[
            &[3.0, 3.2, 9.0, 0.8],
            &[4.0, 4.3, 12.0, 1.0],
            &[5.0, 5.3, 15.0, 1.2],
            &[6.0, 6.4, 18.0, 1.6],
            &[8.0, 8.4, 24.0, 2.0],
            &[10.0, 10.5, 30.0, 2.5],
            &[12.0, 13.0, 37.0, 3.0],
        ],
        lengths: None,
        default: (5.0, 0.0),
        material: "Steel",
        color: ZINC,
        script: r#"
revolve([[row[1] / 2, 0.0], [row[2] / 2, 0.0], [row[2] / 2, row[3]], [row[1] / 2, row[3]]], 360.0)
"#,
    },
    Standard {
        key: "ISO 8734",
        name: "Dowel pin, hardened",
        category: "Pins and rods",
        size_fmt: "Ø{}",
        title: "{k} {s}x{l}",
        rows: &[
            &[2.0, 4.0, 20.0],
            &[2.5, 5.0, 24.0],
            &[3.0, 5.0, 30.0],
            &[4.0, 6.0, 40.0],
            &[5.0, 8.0, 50.0],
            &[6.0, 10.0, 60.0],
            &[8.0, 14.0, 80.0],
            &[10.0, 18.0, 100.0],
            &[12.0, 24.0, 100.0],
        ],
        lengths: Some(PIN_LENGTHS),
        default: (5.0, 20.0),
        material: "Steel",
        color: BRIGHT,
        script: r#"
let r = size / 2; let c = 0.12 * size;
revolve([[0.0, 0.0], [r - c, 0.0], [r, c], [r, length - c], [r - c, length], [0.0, length]], 360.0)
"#,
    },
    Standard {
        key: "Shaft",
        name: "Precision shaft (h6)",
        category: "Pins and rods",
        size_fmt: "Ø{}",
        title: "Shaft {s}x{l}",
        rows: &[
            &[3.0, 20.0, 300.0],
            &[4.0, 20.0, 400.0],
            &[5.0, 20.0, 500.0],
            &[6.0, 20.0, 600.0],
            &[8.0, 20.0, 1000.0],
            &[10.0, 20.0, 1000.0],
            &[12.0, 20.0, 1000.0],
            &[16.0, 20.0, 1000.0],
            &[20.0, 20.0, 1000.0],
            &[25.0, 20.0, 1000.0],
        ],
        lengths: Some(ROD_LENGTHS),
        default: (8.0, 300.0),
        material: "Steel",
        color: BRIGHT,
        script: r#"
let r = size / 2; let c = 0.5;
revolve([[0.0, 0.0], [r - c, 0.0], [r, c], [r, length - c], [r - c, length], [0.0, length]], 360.0)
"#,
    },
    Standard {
        key: "Bearing",
        name: "Deep groove ball bearing",
        category: "Bearings",
        size_fmt: "{}",
        title: "Bearing {s}",
        // designation, bore d, outside D, width B
        rows: &[
            &[623.0, 3.0, 10.0, 4.0],
            &[624.0, 4.0, 13.0, 5.0],
            &[625.0, 5.0, 16.0, 5.0],
            &[626.0, 6.0, 19.0, 6.0],
            &[688.0, 8.0, 16.0, 5.0],
            &[608.0, 8.0, 22.0, 7.0],
            &[609.0, 9.0, 24.0, 7.0],
            &[6800.0, 10.0, 19.0, 5.0],
            &[6801.0, 12.0, 21.0, 5.0],
            &[6000.0, 10.0, 26.0, 8.0],
            &[6001.0, 12.0, 28.0, 8.0],
            &[6002.0, 15.0, 32.0, 9.0],
            &[6003.0, 17.0, 35.0, 10.0],
            &[6004.0, 20.0, 42.0, 12.0],
            &[6005.0, 25.0, 47.0, 12.0],
            &[6200.0, 10.0, 30.0, 9.0],
            &[6201.0, 12.0, 32.0, 10.0],
            &[6202.0, 15.0, 35.0, 11.0],
            &[6203.0, 17.0, 40.0, 12.0],
            &[6204.0, 20.0, 47.0, 14.0],
            &[6205.0, 25.0, 52.0, 15.0],
        ],
        lengths: None,
        default: (608.0, 0.0),
        material: "Steel",
        color: BRIGHT,
        script: r#"
// one solid: inner and outer rings with the shields recessed between them
let ri = row[1] / 2; let ro = row[2] / 2; let b = row[3];
let w = ro - ri; let c = if 0.06 * w < 0.3 { 0.06 * w } else { 0.3 };
let a = ri + 0.28 * w; let e = ro - 0.24 * w; let g = if 0.06 * b < 0.4 { 0.06 * b } else { 0.4 };
revolve([[ri + c, 0.0], [a, 0.0], [a, g], [e, g], [e, 0.0], [ro - c, 0.0], [ro, c], [ro, b - c], [ro - c, b], [e, b], [e, b - g], [a, b - g], [a, b], [ri + c, b], [ri, b - c], [ri, c]], 360.0)
"#,
    },
    Standard {
        key: "Thrust bearing",
        name: "Thrust ball bearing",
        category: "Bearings",
        size_fmt: "{}",
        title: "Thrust bearing {s}",
        // designation, bore d, outside D, height H
        rows: &[
            &[51100.0, 10.0, 24.0, 9.0],
            &[51101.0, 12.0, 26.0, 9.0],
            &[51102.0, 15.0, 28.0, 9.0],
            &[51103.0, 17.0, 30.0, 9.0],
            &[51104.0, 20.0, 35.0, 10.0],
            &[51105.0, 25.0, 42.0, 11.0],
            &[51106.0, 30.0, 47.0, 11.0],
            &[51107.0, 35.0, 52.0, 12.0],
            &[51108.0, 40.0, 60.0, 13.0],
        ],
        lengths: None,
        default: (51104.0, 0.0),
        material: "Steel",
        color: BRIGHT,
        script: r#"
// shaft washer (bore d) below, housing washer (outside D) above, the ball gap between
let ri = row[1] / 2; let ro = row[2] / 2; let h = row[3];
let t = 0.3 * h; let gap = 0.12 * (ro - ri);
revolve([[ri, 0.0], [ro - 0.5, 0.0], [ro - 0.5, t], [ro - gap, t], [ro - gap, h - t], [ro, h - t], [ro, h], [ri + 0.5, h], [ri + 0.5, h - t], [ri + gap, h - t], [ri + gap, t], [ri, t]], 360.0)
"#,
    },
    Standard {
        key: "LMUU",
        name: "Linear ball bushing",
        category: "Bearings",
        size_fmt: "LM{}UU",
        title: "{s}",
        // shaft d, outside D, length L, groove span B, groove width W, groove dia D1
        rows: &[
            &[6.0, 12.0, 19.0, 13.5, 1.1, 11.5],
            &[8.0, 15.0, 24.0, 17.5, 1.1, 14.3],
            &[10.0, 19.0, 29.0, 22.0, 1.3, 18.0],
            &[12.0, 21.0, 30.0, 23.0, 1.3, 20.0],
            &[16.0, 28.0, 37.0, 26.5, 1.6, 27.0],
            &[20.0, 32.0, 42.0, 30.5, 1.6, 30.5],
        ],
        lengths: None,
        default: (8.0, 0.0),
        material: "Steel",
        color: BRIGHT,
        script: r#"
let ri = size / 2; let ro = row[1] / 2; let l = row[2]; let w = row[4]; let rg = row[5] / 2;
let z1 = (l - row[3]) / 2; let z2 = l - z1; let c = 0.4;
revolve([[ri, 0.0], [ro - c, 0.0], [ro, c], [ro, z1], [rg, z1], [rg, z1 + w], [ro, z1 + w], [ro, z2 - w], [rg, z2 - w], [rg, z2], [ro, z2], [ro, l - c], [ro - c, l], [ri, l]], 360.0)
"#,
    },
    Standard {
        key: "NEMA",
        name: "Stepper motor",
        category: "Motors",
        size_fmt: "NEMA {}",
        title: "{s} stepper, {l} mm",
        // size, square, hole pitch, hole dia, hole depth, boss dia, boss height, shaft dia, shaft length, corner chamfer, lengths
        rows: &[
            &[8.0, 20.3, 16.0, 2.0, 2.5, 16.0, 1.5, 4.0, 10.0, 2.0, 20.0, 40.0],
            &[11.0, 28.2, 23.0, 2.5, 3.0, 22.0, 2.0, 5.0, 20.0, 2.5, 30.0, 51.0],
            &[14.0, 35.2, 26.0, 3.0, 3.5, 22.0, 2.0, 5.0, 20.0, 3.5, 20.0, 52.0],
            &[17.0, 42.3, 31.0, 3.0, 4.5, 22.0, 2.0, 5.0, 24.0, 4.0, 20.0, 60.0],
            &[23.0, 56.4, 47.14, 5.1, 6.0, 38.1, 1.6, 6.35, 21.0, 5.0, 41.0, 100.0],
        ],
        lengths: Some(MOTOR_LENGTHS),
        default: (17.0, 40.0),
        material: "Aluminium 6061",
        color: [52, 54, 58],
        script: r#"
let a = row[1] / 2; let p = row[2] / 2; let ch = row[9]; let sd = row[7]; let sl = row[8];
let body = prism([[a - ch, -a], [a, -a + ch], [a, a - ch], [a - ch, a], [-a + ch, a], [-a, a - ch], [-a, -a + ch], [-a + ch, -a]], length).move(0.0, 0.0, -length);
for q in [[1.0, 1.0], [-1.0, 1.0], [-1.0, -1.0], [1.0, -1.0]] {
    body = body - cylinder(row[3] / 2, row[4] + 1.0).move(q[0] * p, q[1] * p, -row[4]);
}
let shaft = cylinder(sd / 2, sl + 1.0).move(0.0, 0.0, -1.0) - cube(sd, sd, 0.7 * sl + 1.0).move(sd / 2 - 0.1 * sd, -sd / 2, 0.3 * sl + 0.01);
body + cylinder(row[5] / 2, row[6] + 0.5).move(0.0, 0.0, -0.5) + shaft
"#,
    },
    Standard {
        key: "Extrusion",
        name: "T-slot aluminium extrusion",
        category: "Profiles",
        size_fmt: "{}",
        title: "Extrusion {s}x{l}",
        // code, width, height, module, slot opening, lip, undercut width, slot depth, centre hole, lengths
        rows: &[
            &[2020.0, 20.0, 20.0, 20.0, 6.2, 1.8, 11.0, 6.1, 4.2, 20.0, 3000.0],
            &[2040.0, 20.0, 40.0, 20.0, 6.2, 1.8, 11.0, 6.1, 4.2, 20.0, 3000.0],
            &[2060.0, 20.0, 60.0, 20.0, 6.2, 1.8, 11.0, 6.1, 4.2, 20.0, 3000.0],
            &[2080.0, 20.0, 80.0, 20.0, 6.2, 1.8, 11.0, 6.1, 4.2, 20.0, 3000.0],
            &[3030.0, 30.0, 30.0, 30.0, 8.2, 2.2, 16.5, 9.0, 6.8, 30.0, 3000.0],
            &[3060.0, 30.0, 60.0, 30.0, 8.2, 2.2, 16.5, 9.0, 6.8, 30.0, 3000.0],
            &[4040.0, 40.0, 40.0, 40.0, 8.2, 4.3, 20.0, 12.2, 6.8, 40.0, 3000.0],
            &[4080.0, 40.0, 80.0, 40.0, 8.2, 4.3, 20.0, 12.2, 6.8, 40.0, 3000.0],
        ],
        lengths: Some(ROD_LENGTHS),
        default: (2020.0, 300.0),
        material: "Aluminium 6061",
        color: BRIGHT,
        script: r#"
let w = row[1]; let h = row[2]; let m = row[3]; let o = row[4] / 2; let lip = row[5]; let u = row[6] / 2; let dep = row[7];
let body = rp(-w / 2, -h / 2, w / 2, h / 2, length);
let nx = (w / m).round().to_int(); let ny = (h / m).round().to_int();
// a slot in the face y = h0 (facing +Y), centred on x = xc
let slot = |xc, h0| [
    rp(xc - o, h0 - lip - 0.01, xc + o, h0 + 1.0, length),
    prism([[xc - o, h0 - dep], [xc + o, h0 - dep], [xc + u, h0 - lip], [xc - u, h0 - lip]], length),
];
let cuts = [];
for i in 0..nx {
    let xc = -w / 2 + m * (i + 0.5);
    for t in slot.call(xc, h / 2) { cuts.push(t); }
    for t in slot.call(-xc, h / 2) { cuts.push(t.rotate_z(180.0)); }
}
for j in 0..ny {
    let yc = -h / 2 + m * (j + 0.5);
    for t in slot.call(-yc, w / 2) { cuts.push(t.rotate_z(-90.0)); }
    for t in slot.call(yc, w / 2) { cuts.push(t.rotate_z(90.0)); }
    for i in 0..nx { cuts.push(cylinder(row[8] / 2, length + 2.0).move(-w / 2 + m * (i + 0.5), yc, -1.0)); }
}
for t in cuts { body = body - t; }
body
"#,
    },
];

pub fn categories() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vec![];
    for s in STANDARDS {
        if !v.contains(&s.category) {
            v.push(s.category);
        }
    }
    v
}

pub fn find(key: &str) -> Option<&'static Standard> {
    let k = key.trim().to_lowercase();
    STANDARDS.iter().find(|s| s.key.to_lowercase() == k).or_else(|| STANDARDS.iter().find(|s| s.name.to_lowercase() == k))
}

pub fn fmt_num(v: f64) -> String {
    let s = format!("{:.3}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Standard {
    pub fn sizes(&self) -> Vec<f64> {
        self.rows.iter().map(|r| r[0]).collect()
    }
    pub fn row(&self, size: f64) -> Option<&'static [f64]> {
        self.rows.iter().copied().find(|r| (r[0] - size).abs() < 1e-6)
    }
    pub fn size_label(&self, size: f64) -> String {
        self.size_fmt.replace("{}", &fmt_num(size))
    }
    /// A size from what the user typed: "M5", "5", "608", "NEMA 17", "LM8UU".
    pub fn parse_size(&self, text: &str) -> Option<f64> {
        let t = text.trim().to_lowercase();
        self.sizes().into_iter().find(|&s| self.size_label(s).to_lowercase() == t || fmt_num(s) == t)
    }
    /// Catalogue lengths for a size.
    pub fn lengths_for(&self, size: f64) -> Vec<f64> {
        let (Some(ls), Some(r)) = (self.lengths, self.row(size)) else { return vec![] };
        let (lo, hi) = (r[r.len() - 2], r[r.len() - 1]);
        ls.iter().copied().filter(|&l| l >= lo - 1e-9 && l <= hi + 1e-9).collect()
    }
    pub fn title(&self, size: f64, length: Option<f64>) -> String {
        self.title.replace("{k}", self.key).replace("{s}", &self.size_label(size)).replace("{l}", &length.map(fmt_num).unwrap_or_default())
    }
    /// Name of the custom feature definition.
    pub fn def_name(&self) -> String {
        format!("{} {}", self.key, self.name.to_lowercase())
    }

    /// The custom feature definition, with the dimension table in the script.
    pub fn def(&self) -> CustomDef {
        let n = if self.lengths.is_some() { 2 } else { 0 };
        let table: Vec<String> = self.rows.iter().map(|r| format!("[{}]", r[..r.len() - n].iter().map(|v| format!("{v:?}")).collect::<Vec<_>>().join(", "))).collect();
        let sizes: Vec<String> = self.sizes().iter().map(|&s| self.size_label(s)).collect();
        let mut src = format!("// {} {} ({}). Generated by the standard parts library; threads are not modelled.\n", self.key, self.name, self.category.to_lowercase());
        src += &format!("let TABLE = [\n    {}\n];\n", table.join(",\n    "));
        src += "let row = [];\nfor r in TABLE { if abs(r[0] - size) < 1e-6 { row = r; } }\n";
        src += &format!("if row.len() == 0 {{ throw \"no size \" + size + \"; the sizes are {}\"; }}\n", sizes.join(", "));
        if self.lengths.is_some() {
            src += "if length <= 0.0 { throw \"the length must be positive\"; }\n";
        }
        src += HELPERS;
        src += self.script;
        let mut params = vec![ParamDef { name: "size".into(), kind: "number".into(), default: fmt_num(self.default.0) }];
        if self.lengths.is_some() {
            params.push(ParamDef { name: "length".into(), kind: "length".into(), default: fmt_num(self.default.1) });
        }
        CustomDef { name: self.def_name(), params, selection: String::new(), source: src }
    }
}

/// Add a standard part to `doc` (the Standard parts Part Studio), or find it if it is already
/// there. Returns the part's body, its name and whether a feature was added. The new part is
/// left showing and the others are hidden, since they all sit at the origin.
pub fn add_to_doc(doc: &mut Document, std: &Standard, size: f64, length: Option<f64>) -> Result<(BodyId, String, bool), String> {
    if std.row(size).is_none() {
        let sizes: Vec<String> = std.sizes().iter().map(|&s| std.size_label(s)).collect();
        return Err(format!("{} {} has no size {}; the sizes are {}", std.key, std.name, fmt_num(size), sizes.join(", ")));
    }
    let length = match (std.lengths.is_some(), length) {
        (true, Some(l)) if l > 0.0 => Some(l),
        (true, _) => return Err(format!("{} {} needs a length", std.key, std.name)),
        (false, _) => None,
    };
    let def = std.def();
    let def_name = def.name.clone();
    // keep a definition already in the document (the user may have edited it)
    if !doc.custom_features.iter().any(|d| d.name == def_name) {
        doc.custom_features.push(def);
    }
    let mut values = std::collections::BTreeMap::new();
    values.insert("size".to_string(), fmt_num(size));
    if let Some(l) = length {
        values.insert("length".to_string(), fmt_num(l));
    }
    let title = std.title(size, length);
    let existing = doc.features.iter().find(|f| matches!(&f.op, Op::Custom { name, values: v, .. } if *name == def_name && *v == values)).map(|f| f.id);
    let (fid, added) = match existing {
        Some(id) => (id, false),
        None => (doc.insert(title.clone(), Op::Custom { name: def_name, values, refs: vec![], select: String::new(), op: BoolOp::New }), true),
    };
    let key = crate::doc::body_key((fid, 0));
    for (k, p) in doc.parts.iter_mut() {
        p.hidden = *k != key;
    }
    let p = doc.parts.entry(key).or_insert_with(|| PartProps { name: Some(title.clone()), color: Some(std.color), material: Some(std.material.to_string()), ..Default::default() });
    p.hidden = false;
    let name = p.name.clone().unwrap_or(title);
    Ok(((fid, 0), name, added))
}

/// Index of the Standard parts Part Studio, made if there is none.
pub fn studio_index(ws: &mut crate::workspace::Workspace) -> usize {
    if let Some(i) = ws.studios.iter().position(|s| s.name == STUDIO) {
        return i;
    }
    let units = ws.studios.first().map(|s| s.doc.units).unwrap_or_default();
    ws.studios.push(crate::workspace::Studio { name: STUDIO.into(), doc: Document { units, ..Default::default() } });
    ws.studios.len() - 1
}

/// The catalogue as JSON, for the agent tools.
pub fn catalogue_json() -> serde_json::Value {
    serde_json::Value::Array(
        STANDARDS
            .iter()
            .map(|s| {
                serde_json::json!({
                    "standard": s.key,
                    "name": s.name,
                    "category": s.category,
                    "sizes": s.sizes().iter().map(|&v| s.size_label(v)).collect::<Vec<_>>(),
                    "length": s.lengths.is_some(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every standard's default size builds one solid with a sensible volume.
    #[test]
    fn every_standard_builds() {
        for s in STANDARDS {
            let def = s.def();
            let mut vals = vec![("size".to_string(), crate::script::Value::Num(s.default.0))];
            if s.lengths.is_some() {
                vals.push(("length".to_string(), crate::script::Value::Num(s.default.1)));
            }
            let (shapes, _) = crate::script::run(&def, vals, &[]).unwrap_or_else(|e| panic!("{}: {e}", s.key));
            assert_eq!(shapes.len(), 1, "{}", s.key);
            let v = crate::kernel::volume(&shapes[0]);
            assert!(v > 1.0, "{}: volume {v}", s.key);
        }
    }

    #[test]
    fn every_size_builds() {
        for s in STANDARDS {
            for size in s.sizes() {
                let def = s.def();
                let mut vals = vec![("size".to_string(), crate::script::Value::Num(size))];
                if s.lengths.is_some() {
                    let l = s.lengths_for(size).get(1).copied().unwrap_or(s.default.1);
                    vals.push(("length".to_string(), crate::script::Value::Num(l)));
                }
                let (shapes, _) = crate::script::run(&def, vals, &[]).unwrap_or_else(|e| panic!("{} {size}: {e}", s.key));
                let solids: usize = shapes.iter().map(|x| crate::kernel::solids_of(x).len()).sum();
                assert_eq!(solids, 1, "{} {size}", s.key);
            }
        }
    }

    #[test]
    fn unknown_size_is_an_error() {
        let s = find("ISO 4762").unwrap();
        let mut doc = Document::default();
        assert!(add_to_doc(&mut doc, s, 7.0, Some(10.0)).is_err());
        let (b1, n1, added) = add_to_doc(&mut doc, s, 5.0, Some(16.0)).unwrap();
        assert!(added);
        assert_eq!(n1, "ISO 4762 M5x16");
        let (b2, _, added) = add_to_doc(&mut doc, s, 5.0, Some(16.0)).unwrap();
        assert!(!added);
        assert_eq!(b1, b2);
        assert_eq!(s.parse_size("m5"), Some(5.0));
        assert_eq!(find("bearing").unwrap().parse_size("608"), Some(608.0));
    }
}
