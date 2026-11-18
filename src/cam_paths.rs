//! 2.5D toolpaths. The part is looked at from the spindle: at each height the
//! area the tool must stay out of is the "shadow" of the part above it (the
//! union of its upward triangles clipped to that height, projected onto XY).
//! Pockets are the holes in that shadow; clearing works on the stock outline
//! minus it; contours follow its offset. Offsets and booleans use i_overlay.

use std::cell::RefCell;
use std::collections::HashMap;

use glam::{DMat3, DVec2, DVec3};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::overlay::FloatOverlay;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::float::outline::offset::OutlineOffset;
use i_overlay::mesh::float::style::{LineJoin, OutlineStyle};
use serde::{Deserialize, Serialize};

use crate::cam::{Cam, OpKind, Operation, Origin, Side, Tool, ToolKind, METRIC_COARSE};
use crate::cam_sim::Sim;
use crate::cam_stock::StockMap;

pub type P2 = [f64; 2];
pub type Contour = Vec<P2>;
pub type Shape = Vec<Contour>;
pub type Shapes = Vec<Shape>;

/// A tool motion in setup coordinates (machine axes, before the work offset).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum Move {
    Rapid([f64; 3]),
    /// Linear cut at a feed (mm/min).
    Feed([f64; 3], f64),
    /// Tapping cycle at the current XY: down to `z`, spindle reversed, back up to `r`.
    Tap {
        z: f64,
        r: f64,
        pitch: f64,
    },
    /// Pause in seconds.
    Dwell(f64),
}

impl Move {
    pub fn end(&self, from: DVec3) -> DVec3 {
        match *self {
            Move::Rapid(p) | Move::Feed(p, _) => DVec3::from(p),
            Move::Tap { r, .. } => DVec3::new(from.x, from.y, r),
            Move::Dwell(_) => from,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Toolpath {
    pub op: u64,
    pub tool: u64,
    pub moves: Vec<Move>,
    pub warnings: Vec<String>,
    /// Lengths in mm and estimated time in seconds.
    pub cut_length: f64,
    pub rapid_length: f64,
    pub time: f64,
    /// A probing operation's cycle, which the post writes instead of the moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<crate::cam_probe::Plan>,
    /// Its moves carry on from where the toolpath before it ended (`tighten_links`): the post doesn't lift the
    /// tool to the safe height before them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked: bool,
    /// What the feed schedule did to it (`cam_feed_schedule`), when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_report: Option<crate::cam_feed_schedule::Report>,
}

// ---------------------------------------------------------------- part geometry

/// Rotation taking part coordinates to setup coordinates, so that `orient` points up.
pub fn orient_matrix(orient: &str) -> DMat3 {
    let rows = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| DMat3::from_cols_array(&[a[0], b[0], c[0], a[1], b[1], c[1], a[2], b[2], c[2]]);
    match orient {
        "bottom" => rows([1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]),
        "front" => rows([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
        "back" => rows([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        "right" => rows([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        "left" => rows([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]),
        _ => DMat3::IDENTITY,
    }
}

/// Rotation taking part coordinates to a setup's coordinates: its orientation, then its 3+2 tilt.
pub fn setup_matrix(cam: &Cam) -> DMat3 {
    let [a, b] = cam.tilt;
    DMat3::from_rotation_y(b.to_radians()) * DMat3::from_rotation_x(a.to_radians()) * orient_matrix(&cam.orient)
}

/// A circular edge: centre, axis, radius.
pub type Circle = ([f64; 3], [f64; 3], f64);

/// A sketch's curves, as polylines (closed ones flagged), for the operations that follow or carve them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SketchPaths {
    /// The sketch feature's id in the Part Studio.
    pub id: u64,
    pub name: String,
    pub paths: Vec<(Vec<DVec3>, bool)>,
}

/// What a setup machines, in part coordinates: the parts' triangles and circular edges, and the sketches.
#[derive(Clone, Debug, Default)]
pub struct PartInput {
    pub tris: Vec<[DVec3; 3]>,
    pub circles: Vec<Circle>,
    pub sketches: Vec<SketchPaths>,
}

/// The parts being machined, in setup coordinates.
pub struct Part {
    pub tris: Vec<[DVec3; 3]>,
    pub circles: Vec<(DVec3, DVec3, f64)>,
    pub lo: DVec3,
    pub hi: DVec3,
    /// Part → setup rotation.
    pub rot: DMat3,
    pub sketches: Vec<SketchPaths>,
}

impl Part {
    #[cfg(test)]
    pub fn new(tris: &[[DVec3; 3]], circles: &[Circle], orient: &str) -> Part {
        Part::rotated(tris, circles, orient_matrix(orient))
    }

    pub fn rotated(tris: &[[DVec3; 3]], circles: &[Circle], rot: DMat3) -> Part {
        let tris: Vec<[DVec3; 3]> = tris.iter().map(|t| t.map(|p| rot * p)).collect();
        let circles = circles.iter().map(|c| (rot * DVec3::from(c.0), rot * DVec3::from(c.1), c.2)).collect();
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in &tris {
            for p in t {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        if !lo.x.is_finite() {
            lo = DVec3::ZERO;
            hi = DVec3::ZERO;
        }
        Part { tris, circles, lo, hi, rot, sketches: vec![] }
    }

    /// The parts as `cam` sets them up.
    pub fn from_input(input: &PartInput, cam: &Cam) -> Part {
        let mut p = Part::rotated(&input.tris, &input.circles, setup_matrix(cam));
        let rot = p.rot;
        p.sketches = input
            .sketches
            .iter()
            .map(|s| SketchPaths { id: s.id, name: s.name.clone(), paths: s.paths.iter().map(|(pts, closed)| (pts.iter().map(|q| rot * *q).collect(), *closed)).collect() })
            .collect();
        p
    }

    /// Setup → part coordinates.
    pub fn to_part(&self, p: DVec3) -> DVec3 {
        self.rot.transpose() * p
    }
}

/// Stock box in setup coordinates.
pub fn stock_box(cam: &Cam, part: &Part) -> (DVec3, DVec3) {
    let s = &cam.stock;
    if let Some([a, b]) = s.carried {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for k in 0..8 {
            let p = part.rot * DVec3::new([a[0], b[0]][k & 1], [a[1], b[1]][(k >> 1) & 1], [a[2], b[2]][(k >> 2) & 1]);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        return (lo, hi);
    }
    match s.size {
        Some(sz) => {
            let c = (part.lo + part.hi) / 2.0;
            let z0 = part.lo.z - s.bottom;
            (DVec3::new(c.x - sz[0] / 2.0, c.y - sz[1] / 2.0, z0), DVec3::new(c.x + sz[0] / 2.0, c.y + sz[1] / 2.0, z0 + sz[2]))
        }
        None => (part.lo - DVec3::new(s.side, s.side, s.bottom), part.hi + DVec3::new(s.side, s.side, s.top)),
    }
}

/// The corners of the stock carried from an earlier setup when this setup tilts it off square (3+2): its
/// bounding box is then bigger than the block, and its corners are points in the air.
pub fn tilted_block(cam: &Cam, part: &Part) -> Option<[DVec3; 8]> {
    let [a, b] = cam.stock.carried?;
    if part.rot.to_cols_array().iter().all(|v| v.abs() < 1e-9 || (v.abs() - 1.0).abs() < 1e-9) {
        return None;
    }
    Some(std::array::from_fn(|k| part.rot * DVec3::new([a[0], b[0]][k & 1], [a[1], b[1]][(k >> 1) & 1], [a[2], b[2]][(k >> 2) & 1])))
}

/// The highest corner of a tilted block (front-left of equally high ones).
fn top_corner(c: &[DVec3; 8]) -> DVec3 {
    let zmax = c.iter().map(|p| p.z).fold(f64::NEG_INFINITY, f64::max);
    *c.iter().filter(|p| p.z > zmax - 1e-6).min_by(|a, b| (a.x + a.y).total_cmp(&(b.x + b.y))).unwrap()
}

/// The work origin (G54 zero) in setup coordinates. On a tilted block it is a point on the block, not on its
/// bounding box: the highest corner, the middle of the face that looks most up, or the lowest corner.
pub fn work_origin(cam: &Cam, part: &Part) -> DVec3 {
    if let Some(c) = tilted_block(cam, part) {
        match cam.origin {
            Origin::StockTopCorner => return top_corner(&c),
            Origin::StockTopCenter => {
                // the middle of the face that looks most up (faces by their corner bits: bit `b` of the corner index at `v`)
                let up = |b: usize, v: usize| (part.rot * DVec3::AXES[b]).z * if v == 1 { 1.0 } else { -1.0 };
                let (b, v) = (0..3).flat_map(|b| [(b, 0), (b, 1)]).max_by(|x, y| up(x.0, x.1).total_cmp(&up(y.0, y.1))).unwrap();
                return (0..8).filter(|k| (k >> b) & 1 == v).map(|k| c[k]).sum::<DVec3>() / 4.0;
            }
            Origin::StockBottomCorner => return *c.iter().min_by(|a, b| a.z.total_cmp(&b.z)).unwrap(),
            _ => {}
        }
    }
    let (lo, hi) = stock_box(cam, part);
    match cam.origin {
        Origin::StockTopCorner => DVec3::new(lo.x, lo.y, hi.z),
        Origin::StockTopCenter => DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, hi.z),
        Origin::StockBottomCorner => lo,
        Origin::StockBottomCenter => DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, lo.z),
        Origin::PartBottomCenter => DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, part.lo.z),
        Origin::PartTopCorner => DVec3::new(part.lo.x, part.lo.y, part.hi.z),
        Origin::Model => DVec3::ZERO,
    }
}

/// Where the work zero is, for the operator; on a tilted block also where its highest corner is from there,
/// a point to touch off on.
pub fn work_zero_note(cam: &Cam, part: &Part) -> String {
    let Some(c) = tilted_block(cam, part) else { return cam.origin.label().to_lowercase() };
    let o = work_origin(cam, part);
    let what = match cam.origin {
        Origin::StockTopCorner => "the highest corner of the tilted stock".to_string(),
        Origin::StockTopCenter => "the middle of the tilted stock's upper face, on its surface".to_string(),
        Origin::StockBottomCorner => "the lowest corner of the tilted stock".to_string(),
        other => other.label().to_lowercase(),
    };
    let t = top_corner(&c) - o;
    if t.length() < 1e-6 {
        what
    } else {
        let f = crate::cam::fmt_mm;
        format!("{what}; the stock's highest corner is at X{} Y{} Z{} from it", f(t.x), f(t.y), f(t.z))
    }
}

// ---------------------------------------------------------------- 2D helpers

pub(crate) fn flat(s: &Shapes) -> Vec<Contour> {
    s.iter().flat_map(|sh| sh.iter().cloned()).collect()
}

/// The area inside an odd number of the loops (text: the holes of letters stay open).
pub fn even_odd(contours: &[Contour]) -> Shapes {
    if contours.is_empty() {
        return vec![];
    }
    let mut ov = FloatOverlay::with_subj(contours);
    clean(ov.overlay(OverlayRule::Subject, FillRule::EvenOdd))
}

pub fn union_all(contours: &[Contour]) -> Shapes {
    if contours.is_empty() {
        return vec![];
    }
    let mut ov = FloatOverlay::with_subj(contours);
    clean(ov.overlay(OverlayRule::Subject, FillRule::NonZero))
}

pub fn intersection(a: &Shapes, b: &Shapes) -> Shapes {
    if a.is_empty() || b.is_empty() {
        return vec![];
    }
    clean(flat(a).overlay(&flat(b), OverlayRule::Intersect, FillRule::NonZero))
}

pub fn difference(a: &Shapes, b: &Shapes) -> Shapes {
    if a.is_empty() || b.is_empty() {
        return a.clone();
    }
    clean(flat(a).overlay(&flat(b), OverlayRule::Difference, FillRule::NonZero))
}

/// Grow (d > 0) or shrink (d < 0) shapes with round corners.
pub fn offset(s: &Shapes, d: f64) -> Shapes {
    if s.is_empty() || d.abs() < 1e-9 {
        return s.clone();
    }
    let tol = 0.004;
    let a = (2.0 * (1.0 - (tol / d.abs()).min(1.0)).acos()).clamp(0.04, 0.5);
    let style = OutlineStyle::new(d).line_join(LineJoin::Round(a));
    clean(s.outline(&style))
}

/// Drop degenerate contours and near-duplicate points.
fn clean(s: Shapes) -> Shapes {
    let mut out = vec![];
    for sh in s {
        let mut cs: Vec<Contour> = vec![];
        for (i, c) in sh.into_iter().enumerate() {
            let c = simplify(&c, 0.001);
            if c.len() < 3 || area(&c).abs() < 1e-6 {
                if i == 0 {
                    break;
                }
                continue;
            }
            cs.push(c);
        }
        if !cs.is_empty() {
            out.push(cs);
        }
    }
    out
}

pub fn area(c: &[P2]) -> f64 {
    let n = c.len();
    (0..n).map(|i| c[i][0] * c[(i + 1) % n][1] - c[(i + 1) % n][0] * c[i][1]).sum::<f64>() / 2.0
}

pub fn shapes_area(s: &Shapes) -> f64 {
    s.iter().flat_map(|sh| sh.iter()).map(|c| area(c)).sum()
}

/// Remove points closer than `tol` to the line through their neighbours (closed contour).
fn simplify(c: &[P2], tol: f64) -> Contour {
    let mut v: Contour = c.to_vec();
    if v.len() < 4 {
        return v;
    }
    loop {
        let n = v.len();
        if n < 4 {
            return v;
        }
        let mut keep = vec![true; n];
        let mut removed = false;
        let mut i = 0;
        while i < n {
            let a = DVec2::from(v[(i + n - 1) % n]);
            let b = DVec2::from(v[i]);
            let cc = DVec2::from(v[(i + 1) % n]);
            let ac = cc - a;
            let dist = if ac.length() < 1e-12 { (b - a).length() } else { (b - a).perp_dot(ac).abs() / ac.length() };
            if dist < tol && (b - a).length() < 5.0 && keep[(i + n - 1) % n] {
                keep[i] = false;
                removed = true;
                i += 2;
            } else {
                i += 1;
            }
        }
        if !removed {
            return v;
        }
        v = v.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect();
    }
}

fn winding(c: &[P2], p: P2) -> i32 {
    let mut w = 0;
    let n = c.len();
    for i in 0..n {
        let a = c[i];
        let b = c[(i + 1) % n];
        if a[1] <= p[1] {
            if b[1] > p[1] && (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]) > 0.0 {
                w += 1;
            }
        } else if b[1] <= p[1] && (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]) < 0.0 {
            w -= 1;
        }
    }
    w
}

pub fn inside(s: &Shapes, p: P2) -> bool {
    s.iter().flat_map(|sh| sh.iter()).map(|c| winding(c, p)).sum::<i32>() != 0
}

fn shape_inside(sh: &Shape, p: P2) -> bool {
    sh.iter().map(|c| winding(c, p)).sum::<i32>() != 0
}

fn perimeter(c: &[P2]) -> f64 {
    let n = c.len();
    (0..n).map(|i| DVec2::from(c[i]).distance(DVec2::from(c[(i + 1) % n]))).sum()
}

pub(crate) fn rect(lo: DVec3, hi: DVec3) -> Shapes {
    vec![vec![vec![[lo.x, lo.y], [hi.x, lo.y], [hi.x, hi.y], [lo.x, hi.y]]]]
}

/// Holes of the shapes, filled, minus any islands inside them.
fn enclosed(s: &Shapes) -> Shapes {
    let holes: Vec<Contour> = s.iter().flat_map(|sh| sh.iter().skip(1)).map(|c| c.iter().rev().copied().collect()).collect();
    difference(&union_all(&holes), s)
}

// ---------------------------------------------------------------- shadows

pub struct Geo<'a> {
    pub part: &'a Part,
    up: Vec<[DVec3; 3]>,
    /// A point on each flat upward triangle.
    floor_pts: Vec<DVec3>,
    cache: RefCell<HashMap<i64, Shapes>>,
}

/// Material counts as "above" a height when it is this much higher.
const EPS: f64 = 1e-3;

impl<'a> Geo<'a> {
    pub fn new(part: &'a Part) -> Geo<'a> {
        let up: Vec<[DVec3; 3]> = part.tris.iter().filter(|t| (t[1] - t[0]).cross(t[2] - t[0]).z > 1e-12).copied().collect();
        let floor_pts = up.iter().filter(|t| (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero().z > 0.9999).map(|t| (t[0] + t[1] + t[2]) / 3.0).collect();
        Geo { part, up, floor_pts, cache: RefCell::new(HashMap::new()) }
    }

    /// Outline (seen from above) of all material higher than z.
    pub fn shadow(&self, z: f64) -> Shapes {
        let key = (z * 1e4).round() as i64;
        if let Some(s) = self.cache.borrow().get(&key) {
            return s.clone();
        }
        let zc = z + EPS;
        let mut polys: Vec<Contour> = vec![];
        for t in &self.up {
            if t[0].z.max(t[1].z).max(t[2].z) <= zc {
                continue;
            }
            // clip to z >= zc
            let mut pts: Vec<DVec3> = vec![];
            for i in 0..3 {
                let a = t[i];
                let b = t[(i + 1) % 3];
                if a.z >= zc {
                    pts.push(a);
                }
                if (a.z >= zc) != (b.z >= zc) {
                    let f = (zc - a.z) / (b.z - a.z);
                    pts.push(a + (b - a) * f);
                }
            }
            if pts.len() < 3 {
                continue;
            }
            let c: Contour = pts.iter().map(|p| [p.x, p.y]).collect();
            if area(&c).abs() < 1e-9 {
                continue;
            }
            polys.push(c);
        }
        let s = union_all(&polys);
        self.cache.borrow_mut().insert(key, s.clone());
        s
    }

    /// Heights of flat upward faces (floors and tops).
    pub fn floors(&self) -> Vec<f64> {
        let mut zs: Vec<f64> = vec![];
        for t in &self.up {
            let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
            if n.z > 0.9999 {
                let z = (t[0].z + t[1].z + t[2].z) / 3.0;
                if !zs.iter().any(|q| (q - z).abs() < 1e-3) {
                    zs.push(z);
                }
            }
        }
        zs.sort_by(|a, b| b.partial_cmp(a).unwrap());
        zs
    }

    /// Vertical holes open from above: centre, radius, top, bottom, through.
    pub fn holes(&self) -> Vec<Hole> {
        let mut groups: Vec<Hole> = vec![];
        for (c, axis, r) in &self.part.circles {
            if axis.normalize_or_zero().z.abs() < 0.999 || *r < 0.05 {
                continue;
            }
            match groups.iter_mut().find(|h| (h.x - c.x).abs() < 0.01 && (h.y - c.y).abs() < 0.01 && (h.r - r).abs() < 0.01) {
                Some(h) => {
                    h.top = h.top.max(c.z);
                    h.bottom = h.bottom.min(c.z);
                }
                None => groups.push(Hole { x: c.x, y: c.y, r: *r, top: c.z, bottom: c.z, through: false }),
            }
        }
        let mut out = vec![];
        for mut h in groups {
            if h.top - h.bottom < 0.01 {
                continue;
            }
            let sh = self.shadow(h.bottom);
            let probe = [[0.0, 0.0], [0.9, 0.0], [-0.9, 0.0], [0.0, 0.9], [0.0, -0.9]];
            if probe.iter().any(|d| inside(&sh, [h.x + d[0] * h.r, h.y + d[1] * h.r])) {
                continue;
            }
            // a whole hole has material all round it (a rounded pocket corner doesn't)
            let wall = self.shadow((h.top + h.bottom) / 2.0);
            let ring = (0..8).map(|k| k as f64 * std::f64::consts::FRAC_PI_4).map(|a| [h.x + a.cos() * (h.r + 0.05), h.y + a.sin() * (h.r + 0.05)]);
            if !ring.into_iter().all(|q| inside(&wall, q)) {
                continue;
            }
            h.through = h.bottom <= self.part.lo.z + 0.01;
            out.push(h);
        }
        out.sort_by(|a, b| (a.x, a.y).partial_cmp(&(b.x, b.y)).unwrap());
        out
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hole {
    pub x: f64,
    pub y: f64,
    pub r: f64,
    pub top: f64,
    pub bottom: f64,
    pub through: bool,
}

/// Cutting levels from just below `top` down to `bottom`, including the given floors in between.
pub(crate) fn levels(top: f64, bottom: f64, stepdown: f64, floors: &[f64]) -> Vec<f64> {
    if top <= bottom + 1e-6 {
        return vec![bottom];
    }
    let sd = stepdown.max(0.01);
    let n = ((top - bottom) / sd - 1e-6).ceil().max(1.0) as usize;
    let mut v: Vec<f64> = (1..=n).map(|i| (top - sd * i as f64).max(bottom)).collect();
    for f in floors {
        if *f > bottom + 1e-6 && *f < top - 1e-6 {
            v.push(*f);
        }
    }
    v.sort_by(|a, b| b.partial_cmp(a).unwrap());
    v.dedup_by(|a, b| (*a - *b).abs() < 0.005);
    v
}

// ---------------------------------------------------------------- path builder

pub(crate) struct Pb<'a> {
    pub tool: &'a Tool,
    pub moves: Vec<Move>,
    pub pos: Option<DVec3>,
    pub safe: f64,
    pub retract: f64,
    pub ramp: f64,
    pub feed: f64,
    pub plunge: f64,
}

impl<'a> Pb<'a> {
    pub(crate) fn rapid(&mut self, p: DVec3) {
        if self.pos.map(|q| q.distance(p) > 1e-6).unwrap_or(true) {
            self.moves.push(Move::Rapid(p.to_array()));
            self.pos = Some(p);
        }
    }

    pub(crate) fn cut(&mut self, p: DVec3, f: f64) {
        if self.pos.map(|q| q.distance(p) > 1e-6).unwrap_or(true) {
            self.moves.push(Move::Feed(p.to_array(), f));
            self.pos = Some(p);
        }
    }

    pub(crate) fn up(&mut self) {
        if let Some(p) = self.pos {
            if p.z < self.safe - 1e-6 {
                self.rapid(DVec3::new(p.x, p.y, self.safe));
            }
        }
    }

    /// Retract, move over (x, y) and come down to just above `z_top`.
    pub(crate) fn approach(&mut self, x: f64, y: f64, z_top: f64) {
        self.up();
        self.rapid(DVec3::new(x, y, self.safe));
        self.rapid(DVec3::new(x, y, (z_top + self.retract).min(self.safe)));
    }

    /// Cut a closed loop at `z`, entering by a ramp along it from `z_from`.
    /// When the tool is within `link` of the start at `z_from` it feeds straight over instead of retracting
    /// (if `region` is given the link must stay inside it).
    /// `tabs`: (count, width, top) raises the cut over holding tabs.
    pub(crate) fn cut_loop(&mut self, c: &[P2], z: f64, z_from: f64, link: f64, region: Option<&Shapes>, tabs: Option<(u32, f64, f64)>) {
        if c.len() < 2 {
            return;
        }
        let n = c.len();
        // start nearest the tool; with tabs, start in the middle of a gap (tabs are placed from there)
        let start = match self.pos {
            Some(p) => (0..n).min_by(|a, b| DVec2::from(c[*a]).distance_squared(p.truncate()).partial_cmp(&DVec2::from(c[*b]).distance_squared(p.truncate())).unwrap()).unwrap(),
            None => 0,
        };
        let pts: Vec<P2> = (0..n).map(|i| c[(start + i) % n]).collect();
        let per = perimeter(&pts);
        let s0 = DVec3::new(pts[0][0], pts[0][1], z_from);
        let linked = match self.pos {
            Some(p) if (p.z - z_from).abs() < 1e-6 && p.truncate().distance(s0.truncate()) <= link => {
                let ok = region
                    .map(|r| {
                        (1..8).all(|k| {
                            let q = p.truncate().lerp(s0.truncate(), k as f64 / 8.0);
                            inside(r, [q.x, q.y])
                        })
                    })
                    .unwrap_or(true);
                ok
            }
            _ => false,
        };
        if linked {
            self.cut(s0, self.feed);
        } else {
            self.approach(s0.x, s0.y, z_from);
            self.cut(s0, self.plunge);
        }
        let dz = z_from - z;
        let mut ramp_len = if dz > 1e-6 { dz / self.ramp.tan().max(1e-3) } else { 0.0 };
        if ramp_len > per * 20.0 || per < self.tool.diameter * 0.1 {
            // too tight to ramp along the loop: spiral down inside it, or plunge when there's no room for that either
            if !self.helix_in(&pts, z_from, z) {
                self.cut(DVec3::new(s0.x, s0.y, z), self.plunge);
            }
            ramp_len = 0.0;
        }
        let total = ramp_len + per;
        let z_at = |s: f64| if s < ramp_len { z_from - dz * s / ramp_len } else { z };
        // tab intervals on [0, per)
        let tab_iv: Vec<(f64, f64)> = match tabs {
            Some((count, w, _)) if count > 0 && per > (w + self.tool.diameter) * count as f64 => {
                let half = (w + self.tool.diameter) / 2.0;
                (0..count).map(|i| per * (i as f64 + 0.5) / count as f64).map(|m| (m - half, m + half)).collect()
            }
            _ => vec![],
        };
        let tab_top = tabs.map(|t| t.2).unwrap_or(f64::NEG_INFINITY);
        let in_tab = |s: f64| {
            let m = s.rem_euclid(per.max(1e-9));
            tab_iv.iter().any(|(a, b)| m > *a + 1e-9 && m < *b - 1e-9)
        };
        let zz = |s: f64, inside_tab: bool| if inside_tab { z_at(s).max(tab_top) } else { z_at(s) };
        // walk the loop, with breakpoints at the ramp end and the tab edges
        let mut s = 0.0;
        let mut i = 0usize;
        let mut cuts: Vec<f64> = vec![ramp_len];
        let laps = (total / per.max(1e-9)).ceil() as i64 + 1;
        for lap in 0..laps {
            for (a, b) in &tab_iv {
                cuts.push(a + lap as f64 * per);
                cuts.push(b + lap as f64 * per);
            }
        }
        cuts.retain(|c| *c > 1e-9 && *c < total - 1e-9);
        cuts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        while s < total - 1e-9 {
            let a = DVec2::from(pts[i % n]);
            let b = DVec2::from(pts[(i + 1) % n]);
            let len = a.distance(b);
            let s_end = (s + len).min(total);
            for &cs in cuts.iter().filter(|c| **c > s + 1e-9 && **c < s_end - 1e-9) {
                let q = a.lerp(b, (cs - s) / len.max(1e-12));
                let before = in_tab(cs - 1e-6);
                let after = in_tab(cs + 1e-6);
                self.cut(DVec3::new(q.x, q.y, zz(cs, before)), self.feed);
                if before != after {
                    self.cut(DVec3::new(q.x, q.y, zz(cs, after)), self.feed);
                }
            }
            let q = if s_end < s + len - 1e-9 { a.lerp(b, (s_end - s) / len.max(1e-12)) } else { b };
            self.cut(DVec3::new(q.x, q.y, zz(s_end, in_tab(s_end - 1e-6))), self.feed);
            s = s_end;
            i += 1;
        }
        // end at the full depth (leaves a tab only at the tab height)
        if let Some(p) = self.pos {
            if !in_tab(total - 1e-6) && (p.z - z).abs() > 1e-6 {
                self.cut(DVec3::new(p.x, p.y, z), self.plunge);
            }
        }
    }

    /// Get down from `z_from` to `z` on a helix inside the closed loop `pts` (tool centres), ending at its first
    /// point. False when the loop is too small for a helix (the caller plunges).
    fn helix_in(&mut self, pts: &[P2], z_from: f64, z: f64) -> bool {
        let n = pts.len() as f64;
        let c = pts.iter().fold(DVec2::ZERO, |a, p| a + DVec2::from(*p)) / n;
        // the helix stays inside the loop: no further from its middle than the nearest point of it
        let m = pts.len();
        let room = (0..m)
            .map(|i| {
                let (a, b) = (DVec2::from(pts[i]), DVec2::from(pts[(i + 1) % m]));
                let ab = b - a;
                let t = if ab.length_squared() > 1e-18 { ((c - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                (a + ab * t).distance(c)
            })
            .fold(f64::INFINITY, f64::min);
        let rh = (room * 0.9).min(self.tool.diameter * 0.25);
        if rh < self.tool.diameter * 0.05 || !inside(&vec![vec![pts.to_vec()]], c.to_array()) {
            return false;
        }
        let pitch = (std::f64::consts::TAU * rh * self.ramp.tan()).max(self.tool.diameter * 0.05);
        self.cut(DVec3::new(c.x + rh, c.y, z_from), self.feed);
        helix(self, c.x, c.y, rh, z_from, z, -pitch, 1.0);
        helix(self, c.x, c.y, rh, z, z, 0.0, 1.0);
        self.cut(DVec3::new(pts[0][0], pts[0][1], z), self.feed);
        true
    }

    /// Clear the tool-centre area at depth z, growing the cleared ground a stepover at a time so no pass takes
    /// more than the stepover sideways, also in the corners and along the outer edge: from beside the stock where
    /// the tool can start there, else from loops round each region's core (ramping in); passes skip what's
    /// already clear. `material`: the stock at this level.
    fn clear_area(&mut self, area: &Shapes, material: &Shapes, z: f64, z_from: f64, step: f64, climb: bool) {
        let mut comps: Vec<Shape> = area.clone();
        while !comps.is_empty() {
            // nearest region next
            let here = self.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
            let k = (0..comps.len())
                .min_by(|a, b| {
                    let da = comps[*a][0].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                    let db = comps[*b][0].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                    da.partial_cmp(&db).unwrap()
                })
                .unwrap();
            let comp = comps.remove(k);
            let region: Shapes = vec![comp.clone()];
            let (feed, r) = (self.feed, self.tool.radius());
            let mat = crate::cam_3d::intersect(material, &offset(&region, r + 0.01));
            // where the tool can start beside the material (outside the stock, or over ground already cleared),
            // the passes grow from there; otherwise from loops round the region's core, cut first
            let open = shapes_area(&difference(&region, &offset(&mat, r))) > 0.5 * r * r;
            let mut core = if open { vec![] } else { region.clone() };
            for _ in 0..5000 {
                if core.is_empty() {
                    break;
                }
                let next = offset(&core, -step);
                if next.is_empty() {
                    break;
                }
                core = next;
            }
            let mut loops: Vec<Contour> = flat(&core).into_iter().map(|c| if climb { c } else { c.into_iter().rev().collect() }).collect();
            while !loops.is_empty() {
                let here = self.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
                let j = (0..loops.len())
                    .min_by(|a, b| {
                        let da = loops[*a].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                        let db = loops[*b].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                        da.partial_cmp(&db).unwrap()
                    })
                    .unwrap();
                let l = loops.remove(j);
                self.cut_loop(&l, z, z_from, 0.0, None, None);
            }
            let mut lv = crate::cam_3d::Level { pb: &mut *self, z, z_from, r, step, feed, fast: feed, climb, helix: 0.4 * 2.0 * r, slack: 0.0, blocking: Default::default(), hop: None };
            lv.clear(&region, &mat, &core);
        }
    }
}

// ---------------------------------------------------------------- operations

pub struct Ctx<'a> {
    pub cam: &'a Cam,
    pub geo: Geo<'a>,
    pub stock: (DVec3, DVec3),
    pub origin: DVec3,
    /// The stock as the operation finds it, when it machines only what is left.
    pub stock_in: Option<StockMap>,
}

impl<'a> Ctx<'a> {
    pub fn new(cam: &'a Cam, part: &'a Part) -> Ctx<'a> {
        Ctx { cam, geo: Geo::new(part), stock: stock_box(cam, part), origin: work_origin(cam, part), stock_in: None }
    }

    pub(crate) fn part(&self) -> &Part {
        self.geo.part
    }

    pub(crate) fn wz(&self, z: Option<f64>) -> Option<f64> {
        z.map(|z| z + self.origin.z)
    }

    /// Clear of the stock, the parts and the fixtures.
    pub fn safe(&self) -> f64 {
        let fix = fixture_boxes(self.cam, self.part(), self.origin).iter().map(|f| f.2.z).fold(f64::NEG_INFINITY, f64::max);
        self.stock.1.z.max(self.part().hi.z).max(fix) + self.cam.clearance
    }
}

/// The setup's fixtures as boxes in setup coordinates (`origin`: the work zero), soft jaws included.
pub fn fixture_boxes(cam: &Cam, part: &Part, origin: DVec3) -> Vec<(String, DVec3, DVec3)> {
    let mut v: Vec<_> = cam
        .fixtures
        .iter()
        .map(|f| {
            let (a, b) = (DVec3::from(f.min) + origin, DVec3::from(f.max) + origin);
            (f.name.clone(), a.min(b), a.max(b))
        })
        .collect();
    if let Some(j) = crate::cam_jaws::jaws(cam, part) {
        v.extend(crate::cam_jaws::boxes(&j));
    }
    v
}

/// Lift the toolpath over the fixtures: wherever the tool (`r` plus a 1 mm margin) would pass over one lower
/// than `retract` above its top, it goes up there, across, and back down. Returns how many times it lifts.
fn avoid_fixtures(moves: Vec<Move>, boxes: &[(String, DVec3, DVec3)], r: f64, retract: f64, start: DVec3) -> (Vec<Move>, usize) {
    let mut moves = moves;
    let mut lifts = 0;
    // where the moves go, to skip boxes nowhere near them
    let (mut mlo, mut mhi) = (start, start);
    for m in &moves {
        if let Move::Rapid(p) | Move::Feed(p, _) = m {
            mlo = mlo.min(DVec3::from(*p));
            mhi = mhi.max(DVec3::from(*p));
        }
    }
    for (_, lo, hi) in boxes {
        let (blo, bhi) = (lo.truncate() - DVec2::splat(r + 1.0), hi.truncate() + DVec2::splat(r + 1.0));
        let zc = hi.z + retract;
        if mlo.z >= zc || mhi.x <= blo.x || mlo.x >= bhi.x || mhi.y <= blo.y || mlo.y >= bhi.y {
            continue;
        }
        let blocked = |p: DVec3| p.z < zc - 1e-9 && p.x > blo.x && p.x < bhi.x && p.y > blo.y && p.y < bhi.y;
        // split each straight move where it crosses the box's sides or its clearance height
        let mut out: Vec<Move> = vec![];
        let mut pos = start;
        let mut inside = false;
        for m in moves {
            let (to, rapid, f) = match m {
                Move::Rapid(p) => (DVec3::from(p), true, 0.0),
                Move::Feed(p, f) => (DVec3::from(p), false, f),
                Move::Tap { .. } | Move::Dwell(_) => {
                    if !inside {
                        out.push(m);
                    }
                    continue;
                }
            };
            let mut ts = vec![0.0, 1.0];
            let d = to - pos;
            for (a, lim) in [(0usize, blo.x), (0, bhi.x), (1, blo.y), (1, bhi.y)] {
                let da = d.to_array()[a];
                if da.abs() > 1e-12 {
                    ts.push((lim - pos.to_array()[a]) / da);
                }
            }
            if d.z.abs() > 1e-12 {
                ts.push((zc - pos.z) / d.z);
            }
            ts.retain(|t| (0.0..=1.0).contains(t));
            ts.sort_by(|a, b| a.total_cmp(b));
            ts.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
            for w in ts.windows(2) {
                let (a, b) = (pos + d * w[0], pos + d * w[1]);
                let now = blocked((a + b) / 2.0);
                let lift = |p: DVec3| DVec3::new(p.x, p.y, p.z.max(zc));
                let mv = |p: DVec3| if rapid { Move::Rapid(p.to_array()) } else { Move::Feed(p.to_array(), f) };
                if now && !inside {
                    // straight up where it reaches the fixture
                    out.push(Move::Rapid(lift(a).to_array()));
                    lifts += 1;
                } else if !now && inside {
                    // and back down where it leaves
                    out.push(mv(a));
                }
                inside = now;
                // over it, along the path at the clearance height (at the feed rate when cutting: for the
                // holder that height can still be in the stock)
                out.push(mv(if now { lift(b) } else { b }));
            }
            pos = to;
        }
        if inside {
            out.push(Move::Rapid(DVec3::new(pos.x, pos.y, zc.max(pos.z)).to_array()));
        }
        moves = out;
    }
    (moves, lifts)
}

/// Make the toolpaths of every operation that isn't suppressed.
#[cfg(test)]
pub fn generate(cam: &Cam, part: &Part) -> Vec<Toolpath> {
    generate_from(cam, part, None)
}

/// Grid size for tracking the stock between operations: about 0.2 mm cells.
fn stock_resolution(stock: (DVec3, DVec3)) -> usize {
    let s = (stock.1 - stock.0).truncate();
    ((s.x.max(s.y) / 0.2).ceil() as usize).clamp(200, 1500)
}

/// Make the toolpaths starting from `stock`, the material an earlier setup left (None = the full block).
/// Operations that machine only what is left see the stock as the operations before them leave it.
pub fn generate_from(cam: &Cam, part: &Part, stock: Option<&StockMap>) -> Vec<Toolpath> {
    let paths = generate_all(cam, part, stock, false).0;
    let paths = if cam.stay_down { tighten_links(cam, part, stock, paths) } else { paths };
    limit_feeds(cam, part, stock, paths)
}

/// The toolpaths and the stock once they have all run.
pub fn generate_with_stock(cam: &Cam, part: &Part, stock: Option<&StockMap>) -> (Vec<Toolpath>, StockMap) {
    let (paths, left) = generate_all(cam, part, stock, true);
    let left = left.unwrap_or_else(|| final_stock(cam, part, &paths, stock));
    let paths = if cam.stay_down { tighten_links(cam, part, stock, paths) } else { paths };
    (limit_feeds(cam, part, stock, paths), left)
}

/// The power budget of an operation roughing from it (`power_target` set), W at the shaft: (target, cap) at
/// the speed it runs. The cap is the spindle's caution at that speed (its `spindle_caution` share of the
/// power it has there), never over the never-exceed, unless the op gives one (`power_cap`); a target of 0
/// means 60 % of the power the spindle has at that speed (on the DMC2 at F004 = 150: about 670 W at 9k,
/// 540 W at 12k), and no target goes over the cap.
pub fn power_budget(cam: &Cam, op: &Operation, tool: &Tool) -> Option<(f64, f64)> {
    let want = op.power_target?;
    let (lim, _) = cam.machine.cut_limits();
    let rpm = cam.spindle_rpm(tool);
    let have = lim.available(rpm);
    let never = have * lim.never_exceed.clamp(0.1, 1.0);
    let cap = op.power_cap.filter(|c| *c > 0.0).unwrap_or(have * lim.caution.clamp(0.1, 1.0)).min(never);
    let target = if want > 0.0 { want } else { 0.6 * have };
    Some((target.min(cap), cap))
}


/// Feeds set from the simulated cut (`cam_feed_schedule`): the power budget's cap (`power_target`) and the
/// feed schedule (`feed_schedule`).
pub fn limit_feeds(cam: &Cam, part: &Part, stock_in: Option<&StockMap>, paths: Vec<Toolpath>) -> Vec<Toolpath> {
    crate::cam_feed_schedule::apply(cam, part, stock_in, paths)
}

/// The grid the setup's own simulations use.
pub fn sim_resolution(stock: (DVec3, DVec3)) -> usize {
    stock_resolution(stock)
}

/// Lengths and time of a toolpath (after its moves change).
pub fn toolpath_stats(tp: &mut Toolpath, tool: &Tool, cam: &Cam) {
    stats(tp, tool, cam)
}

/// Toolpaths and stock states worked out before, by the hash of everything they depend on, so an edit
/// regenerates only the operations it changes and the rest-machining ones after them.
#[derive(Default)]
struct Memo {
    tick: u64,
    paths: HashMap<u64, (u64, Toolpath)>,
    stocks: HashMap<u64, (u64, StockMap)>,
}

static MEMO: std::sync::Mutex<Option<Memo>> = std::sync::Mutex::new(None);
const MEMO_PATHS: usize = 64;
const MEMO_STOCKS: usize = 16;

fn memo<R>(f: impl FnOnce(&mut Memo) -> R) -> R {
    let mut g = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(Memo::default);
    m.tick += 1;
    f(m)
}

#[cfg(test)]
pub fn forget_paths() {
    *MEMO.lock().unwrap() = None;
}

fn memo_path(key: u64) -> Option<Toolpath> {
    memo(|m| {
        let t = m.tick;
        m.paths.get_mut(&key).map(|e| {
            e.0 = t;
            e.1.clone()
        })
    })
}

fn memo_stock(key: u64) -> Option<StockMap> {
    memo(|m| {
        let t = m.tick;
        m.stocks.get_mut(&key).map(|e| {
            e.0 = t;
            e.1.clone()
        })
    })
}

fn evict<V>(map: &mut HashMap<u64, (u64, V)>, keep: usize) {
    while map.len() > keep {
        let Some(k) = map.iter().min_by_key(|e| e.1 .0).map(|e| *e.0) else { break };
        map.remove(&k);
    }
}

fn keep_path(key: u64, tp: &Toolpath) {
    memo(|m| {
        m.paths.insert(key, (m.tick, tp.clone()));
        evict(&mut m.paths, MEMO_PATHS);
    })
}

fn keep_stock(key: u64, s: &StockMap) {
    memo(|m| {
        m.stocks.insert(key, (m.tick, s.clone()));
        evict(&mut m.stocks, MEMO_STOCKS);
    })
}

fn hash_of(f: impl FnOnce(&mut std::collections::hash_map::DefaultHasher)) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    f(&mut h);
    h.finish()
}

fn part_hash(part: &Part) -> u64 {
    use std::hash::Hash;
    hash_of(|h| {
        for t in &part.tris {
            for p in t {
                p.to_array().map(f64::to_bits).hash(h);
            }
        }
        for (c, n, r) in &part.circles {
            (c.to_array().map(f64::to_bits), n.to_array().map(f64::to_bits), r.to_bits()).hash(h);
        }
        for sk in &part.sketches {
            sk.id.hash(h);
            for (pts, closed) in &sk.paths {
                closed.hash(h);
                for q in pts {
                    q.to_array().map(f64::to_bits).hash(h);
                }
            }
        }
    })
}

fn stock_hash(s: Option<&StockMap>) -> u64 {
    use std::hash::Hash;
    hash_of(|h| {
        if let Some(s) = s {
            (s.x0.to_bits(), s.y0.to_bits(), s.cell.to_bits(), s.nx, s.ny).hash(h);
            for (l, hi) in s.lo.iter().zip(&s.hi) {
                (l.to_bits(), hi.to_bits()).hash(h);
            }
        }
    })
}

fn generate_all(cam: &Cam, part: &Part, stock: Option<&StockMap>, want_stock: bool) -> (Vec<Toolpath>, Option<StockMap>) {
    use std::hash::Hash;
    let ops: Vec<&Operation> = cam.ops.iter().filter(|o| !o.suppressed).collect();
    // contours always look at the stock the operations before them leave, to know how far out it reaches
    let needs = |o: &Operation| ((o.rest || stock.is_some()) && o.kind.uses_stock()) || (o.kind == OpKind::Contour && !o.suppressed);
    // what every operation depends on: the setup's settings, the part, its own tool, and for rest machining the
    // stock the operations before it leave (`prefix`, chained through their keys)
    let base = {
        let mut c = cam.clone();
        c.ops.clear();
        c.tools.clear();
        c.next_id = 0;
        c.program = 0;
        c.name.clear();
        c.follows.clear();
        c.post.clear();
        c.probe = Default::default();
        c.post_text = Default::default();
        let (cj, ph) = (serde_json::to_string(&c).unwrap_or_default(), part_hash(part));
        hash_of(|h| (cj, ph).hash(h))
    };
    let mut prefix = vec![hash_of(|h| (base, stock_hash(stock)).hash(h))];
    let mut keys = vec![];
    for (k, op) in ops.iter().enumerate() {
        let oj = serde_json::to_string(&(op, cam.tool(op.tool))).unwrap_or_default();
        // pencil picks its reference tool from the operations before it
        let before =
            if op.kind == OpKind::Pencil { serde_json::to_string(&ops[..k].iter().map(|o| (o, cam.tool(o.tool))).collect::<Vec<_>>()).unwrap_or_default() } else { String::new() };
        let st = if needs(op) { prefix[k] } else { 0 };
        let key = hash_of(|h| (base, oj, before, st).hash(h));
        keys.push(key);
        prefix.push(hash_of(|h| (prefix[k], key).hash(h)));
    }
    let mut out: Vec<Option<Toolpath>> = keys.iter().map(|k| memo_path(*k)).collect();

    // the operations that don't depend on the stock, side by side
    let todo: Vec<usize> = (0..ops.len()).filter(|k| out[*k].is_none() && !needs(ops[*k])).collect();
    if !todo.is_empty() {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(todo.len());
        let next = std::sync::atomic::AtomicUsize::new(0);
        let done: Vec<(usize, Toolpath)> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|_| {
                    sc.spawn(|| {
                        let ctx = Ctx::new(cam, part);
                        let mut got = vec![];
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(&k) = todo.get(i) else { break };
                            got.push((k, op_path(&ctx, ops[k])));
                        }
                        got
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        for (k, tp) in done {
            keep_path(keys[k], &tp);
            out[k] = Some(tp);
        }
    }
    // then the ones that machine what is left, in order; the simulator picks up from the latest stock known
    let mut ctx = Ctx::new(cam, part);
    let mut sim: Option<(usize, Sim)> = None;
    let mut state = |at: usize, out: &[Option<Toolpath>]| -> StockMap {
        if let Some(m) = memo_stock(prefix[at]) {
            return m;
        }
        if sim.as_ref().map(|s| s.0 > at).unwrap_or(true) {
            let (from, m) = (1..at).rev().find_map(|j| memo_stock(prefix[j]).map(|m| (j, Some(m)))).unwrap_or((0, stock.cloned()));
            let b = stock_box(cam, part);
            let mut s = Sim::new(cam, part, b, &[], stock_resolution(b));
            if let Some(m) = m {
                s.start_from(&m);
            }
            sim = Some((from, s));
        }
        let (from, s) = sim.as_mut().unwrap();
        for tp in out[*from..at].iter().flatten() {
            s.push(cam, tp);
        }
        *from = at;
        s.run_to(usize::MAX, usize::MAX);
        let m = s.stock_map();
        keep_stock(prefix[at], &m);
        m
    };
    for k in 0..ops.len() {
        if out[k].is_some() {
            continue;
        }
        ctx.stock_in = Some(state(k, &out));
        let tp = op_path(&ctx, ops[k]);
        keep_path(keys[k], &tp);
        out[k] = Some(tp);
    }
    let left = want_stock.then(|| state(ops.len(), &out));
    let mut out: Vec<Toolpath> = out.into_iter().flatten().collect();
    machine_checks(cam, &ctx, &mut out);
    if let Some(first) = out.first_mut() {
        for (k, w) in crate::cam_jaws::fit_warnings(cam, part, stock).into_iter().enumerate() {
            first.warnings.insert(k, w);
        }
    }
    (out, left)
}

/// The stock once every toolpath has run, starting from `stock` (None = the full block).
pub fn final_stock(cam: &Cam, part: &Part, paths: &[Toolpath], stock: Option<&StockMap>) -> StockMap {
    let b = stock_box(cam, part);
    let mut s = Sim::new(cam, part, b, paths, stock_resolution(b));
    if let Some(m) = stock {
        s.start_from(m);
    }
    s.run_to(usize::MAX, usize::MAX);
    s.stock_map()
}

/// Warn where an operation goes beyond the machine: travel, feed and spindle speed.
fn machine_checks(cam: &Cam, ctx: &Ctx, paths: &mut [Toolpath]) {
    let m = &cam.machine;
    for tp in paths.iter_mut() {
        let Some(tool) = cam.path_tool(tp.op, tp.tool) else { continue };
        let tool: &Tool = &tool;
        if cam.max_rpm > 0.0 && tool.rpm > cam.max_rpm + 0.5 {
            tp.warnings.push(format!(
                "T{} is set to {} rpm but the spindle tops out at {}: it runs at {} with its feeds scaled to {}% (same chip load)",
                tool.number,
                tool.rpm.round(),
                cam.max_rpm.round(),
                cam.max_rpm.round(),
                (cam.max_rpm / tool.rpm * 100.0).round()
            ));
        }
        // a slow-spindle minimum (or a hand-set speed) can push the cutting speed far past what the tool takes
        if tool.kind != ToolKind::Tap && tool.rpm > 0.0 {
            let vc = std::f64::consts::PI * tool.diameter * cam.spindle_rpm(tool) / 1000.0;
            let limit = crate::cam::cutting_speed(tool, &cam.stock.material);
            if vc > limit * 1.5 {
                tp.warnings.push(format!(
                    "T{} cuts at {} m/min, well above the about {} m/min a {} tool takes in {}: expect a burnt tool; slow the spindle (and feed) down if it can",
                    tool.number,
                    vc.round(),
                    limit.round(),
                    tool.material,
                    cam.stock.material.to_lowercase()
                ));
            }
        }
        if let Some(other) = cam.tools.iter().find(|t| t.number == tool.number && t.id != tool.id) {
            tp.warnings.push(format!("T{} is also the number of {}: the machine can't tell them apart (tool change and length offset H{})", tool.number, other.describe(), tool.number));
        }
        if let Some(e) = tap_danger(cam, tool) {
            tp.warnings.push(e);
        } else if cam.min_rpm > 0.0 && tool.rpm > 0.0 && cam.spindle_rpm(tool) < cam.min_rpm - 0.5 {
            tp.warnings.push(format!("T{} runs at {} rpm, below the spindle's {} rpm minimum (low torque)", tool.number, cam.spindle_rpm(tool).round(), cam.min_rpm.round()));
        }
        if m.max_feed > 0.0 {
            let mut fast = 0.0f64;
            for mv in tp.moves.iter_mut() {
                if let Move::Feed(_, f) = mv {
                    if *f > m.max_feed {
                        fast = fast.max(*f);
                        *f = m.max_feed;
                    }
                }
            }
            if fast > 0.0 {
                tp.warnings.push(format!("feed {} mm/min is above the machine's {}: slowed down", fast.round(), m.max_feed.round()));
                stats(tp, tool, cam);
            }
        }
        if m.travel.iter().any(|t| *t > 0.0) {
            let mut lo = DVec3::splat(f64::INFINITY);
            let mut hi = DVec3::splat(f64::NEG_INFINITY);
            let mut p = DVec3::new(0.0, 0.0, ctx.safe());
            for mv in &tp.moves {
                p = mv.end(p);
                lo = lo.min(p);
                hi = hi.max(p);
            }
            if lo.x.is_finite() {
                let span = hi - lo;
                for (a, name) in ["X", "Y", "Z"].iter().enumerate() {
                    let need = span.to_array()[a];
                    let have = m.travel[a];
                    if have > 0.0 && need > have + 1e-6 {
                        tp.warnings.push(format!("needs {} mm of {name} travel; the machine has {}", crate::cam::fmt_mm(need.round()), crate::cam::fmt_mm(have)));
                    }
                }
            }
        }
    }
}

/// Advice on the order of the operations and on holding the work: things that are legal but likely not meant.
pub fn hints(cam: &Cam, part: &Part, paths: &[Toolpath]) -> Vec<String> {
    let mut v = vec![];
    let ops: Vec<&Operation> = paths.iter().filter(|p| !p.moves.is_empty()).filter_map(|p| cam.op(p.op)).collect();
    // edge breaking before the cuts that make the edges
    for (i, a) in ops.iter().enumerate() {
        if !matches!(a.kind, OpKind::Chamfer | OpKind::Deburr) {
            continue;
        }
        let later: Vec<&str> = ops[i + 1..]
            .iter()
            .filter(|b| matches!(b.kind, OpKind::Face | OpKind::Clear | OpKind::Pocket | OpKind::Contour | OpKind::Adaptive | OpKind::Drill | OpKind::Bore))
            .map(|b| b.name.as_str())
            .collect();
        if !later.is_empty() {
            v.push(format!(
                "{} runs before {}: it chamfers edges those haven't cut yet (grooving the stock, dimpling holes still to be drilled) and they may cut its chamfers away; move it after them",
                a.name,
                later.join(", ")
            ));
        }
    }
    let (lo, hi) = stock_box(cam, part);
    let margin = (part.lo.x - lo.x).min(part.lo.y - lo.y).min(hi.x - part.hi.x).min(hi.y - part.hi.y);
    let held = !cam.fixtures.is_empty() || cam.soft_jaws.is_some();
    let mut below = vec![];
    for tp in paths.iter().filter(|p| !p.moves.is_empty()) {
        let (Some(op), Some(t)) = (cam.op(tp.op), cam.tool(tp.tool)) else { continue };
        if op.kind == OpKind::Contour && op.tabs > 0 && op.side != Side::Inside && margin < t.diameter + op.leave + 1.0 {
            v.push(format!(
                "{}: the tabs hold the part to the stock around it, but the stock reaches only {} mm past the part and the tool is Ø{}: nothing is left outside the cut for them to hold on to; give the stock a wider side margin",
                op.name,
                crate::cam::fmt_mm(margin),
                crate::cam::fmt_mm(t.diameter)
            ));
        }
        let low = tp.moves.iter().filter_map(|m| match m {
            Move::Feed(p, _) => Some(p[2]),
            Move::Tap { z, .. } => Some(*z),
            _ => None,
        });
        let low = low.fold(f64::INFINITY, f64::min);
        if !held && low < lo.z - 0.01 {
            below.push(format!("{} ({} mm)", op.name, crate::cam::fmt_mm(lo.z - low)));
        }
        if op.kind == OpKind::Adaptive && !op.chip_thinning {
            let k = crate::cam::chip_thinning(stepover(op, t) / t.diameter.max(1e-9));
            if k > 1.05 {
                v.push(format!("{} takes a {}% cut: with chip_thinning on its feed would rise ×{k:.2} for the same chip load", op.name, (stepover(op, t) / t.diameter * 100.0).round()));
            }
        }
    }
    if !below.is_empty() {
        v.push(format!(
            "cutting below the stock bottom with no fixtures defined: {}. Lay the stock on a spoilboard or sacrificial plate, and add the clamps or screws that hold it as fixtures so the toolpaths keep clear of them",
            below.join(", ")
        ));
    }
    v
}

/// A tap that the spindle reverses (G84 or M4) at a speed where it has almost no torque: likely a broken tap.
pub fn tap_danger(cam: &Cam, tool: &Tool) -> Option<String> {
    if tool.kind != ToolKind::Tap || tool.tapping_head || cam.min_rpm <= 0.0 || cam.spindle_rpm(tool) >= cam.min_rpm - 0.5 {
        return None;
    }
    Some(format!(
        "T{} taps at {} rpm, below the spindle's {} rpm minimum: a high-speed spindle has almost no torque there and the tapping cycle \
         reverses it at depth, so the tap is likely to break. Hold the tap in a self-reversing tapping head (tool option), or cut the \
         thread with a thread mill",
        tool.number,
        cam.spindle_rpm(tool).round(),
        cam.min_rpm.round()
    ))
}

/// The setups that run before `ci`, oldest first: each one carries on from the one before it.
pub fn setup_chain(cams: &[Cam], ci: usize) -> Result<Vec<usize>, String> {
    let mut chain = vec![];
    let mut cur = ci;
    while !cams[cur].follows.trim().is_empty() {
        let name = cams[cur].follows.trim();
        let Some(prev) = cams.iter().position(|c| c.name == name) else {
            return Err(format!("{} carries on from \"{name}\", but there is no Manufacture tab of that name", cams[cur].name));
        };
        if prev == ci || chain.contains(&prev) {
            return Err(format!("setups follow each other in a loop at \"{name}\""));
        }
        if cams[prev].studio != cams[ci].studio {
            return Err(format!("\"{name}\" machines a different Part Studio"));
        }
        chain.push(prev);
        cur = prev;
    }
    chain.reverse();
    Ok(chain)
}

/// Bring the stock boxes of `ci` and the setups before it up to date (each carries the box of the one it follows),
/// and return the stock the earlier setups leave for it (None for a first setup). Every setup in the chain is
/// taken to machine the same parts as `ci` (see `link_setups_each` when they pick different bodies).
pub fn link_setups(cams: &mut [Cam], ci: usize, input: &PartInput) -> Result<Option<StockMap>, String> {
    link_setups_each(cams, ci, &|_| input.clone())
}

/// `link_setups` with each setup's own parts (`input_of(k)` for setup k): an earlier setup can keep bodies a
/// later one removes, e.g. a carrier or lugs op 1 leaves on and op 2 machines off. Its stock box and what it
/// leaves come from its own parts, and the box is carried in part coordinates, so a later setup that picks fewer
/// bodies still gets the earlier one's stock where it really is.
pub fn link_setups_each(cams: &mut [Cam], ci: usize, input_of: &dyn Fn(usize) -> PartInput) -> Result<Option<StockMap>, String> {
    let chain = setup_chain(cams, ci)?;
    if chain.is_empty() {
        cams[ci].stock.carried = None;
        return Ok(None);
    }
    let mut order = chain.clone();
    order.push(ci);
    cams[order[0]].stock.carried = None;
    let mut stock: Option<StockMap> = None;
    for w in order.windows(2) {
        let (a, b) = (w[0], w[1]);
        let pa = Part::from_input(&input_of(a), &cams[a]);
        let (lo, hi) = stock_box(&cams[a], &pa);
        let (p, q) = (pa.to_part(lo), pa.to_part(hi));
        cams[b].stock.carried = Some([p.min(q).to_array(), p.max(q).to_array()]);
        let (_, left) = generate_with_stock(&cams[a], &pa, stock.as_ref());
        stock = Some(left.reorient(pa.rot, setup_matrix(&cams[b])));
    }
    Ok(stock)
}

pub fn op_path(ctx: &Ctx, op: &Operation) -> Toolpath {
    let mut tp = Toolpath { op: op.id, tool: op.tool, ..Default::default() };
    let Some(tool) = ctx.cam.tool_for(op) else {
        tp.warnings.push("no tool selected".into());
        return tp;
    };
    let tool: &Tool = &tool;
    if !op.kind.accepts(tool.kind) {
        tp.warnings.push(format!("a {} can't be used for {}", tool.kind.label().to_lowercase(), op.kind.label().to_lowercase()));
        return tp;
    }
    if tool.diameter <= 0.0 {
        tp.warnings.push("the tool has no diameter".into());
        return tp;
    }
    if ctx.part().tris.is_empty() {
        tp.warnings.push("no parts to machine".into());
        return tp;
    }
    // above the spindle's top speed it runs at the top speed, with the feeds scaled to keep the chip load
    let slow = if tool.rpm > 0.0 { ctx.cam.spindle_rpm(tool) / tool.rpm } else { 1.0 };
    let mut pb = Pb {
        tool,
        moves: vec![],
        pos: None,
        safe: ctx.safe(),
        retract: ctx.cam.retract.max(0.1),
        ramp: op.ramp_angle.clamp(0.5, 90.0).to_radians(),
        feed: (tool.feed * slow).max(1.0),
        plunge: (tool.plunge_feed * slow).max(1.0),
    };
    if matches!(op.kind, OpKind::Clear | OpKind::Pocket | OpKind::Bore | OpKind::Adaptive) && matches!(tool.kind, ToolKind::Ball) {
        tp.warnings.push("ball end mill: floors will have scallops; use a flat end mill to finish them".into());
    }
    match op.kind {
        OpKind::Face => face(ctx, op, &mut pb, &mut tp),
        OpKind::Clear => clear(ctx, op, &mut pb, &mut tp),
        OpKind::Pocket => pocket(ctx, op, &mut pb, &mut tp),
        OpKind::Contour => contour(ctx, op, &mut pb, &mut tp),
        OpKind::Chamfer => chamfer(ctx, op, &mut pb, &mut tp),
        OpKind::Drill | OpKind::Bore | OpKind::Thread | OpKind::Tap => holes_op(ctx, op, &mut pb, &mut tp),
        OpKind::Adaptive => crate::cam_3d::adaptive(ctx, op, &mut pb, &mut tp),
        OpKind::Parallel => crate::cam_3d::parallel(ctx, op, &mut pb, &mut tp),
        OpKind::Waterline => crate::cam_3d::waterline(ctx, op, &mut pb, &mut tp),
        OpKind::Scallop => crate::cam_3d::scallop(ctx, op, &mut pb, &mut tp),
        OpKind::Deburr => crate::cam_3d::deburr(ctx, op, &mut pb, &mut tp),
        OpKind::Pencil => crate::cam_3d::pencil(ctx, op, &mut pb, &mut tp),
        OpKind::Trace => crate::cam_engrave::trace(ctx, op, &mut pb, &mut tp),
        OpKind::VCarve => crate::cam_engrave::vcarve(ctx, op, &mut pb, &mut tp),
        OpKind::Probe => crate::cam_probe::probe(ctx, op, &mut pb, &mut tp),
    }
    pb.up();
    // deepest cut below the stock top against the flute length
    if !tool.kind.is_hole_tool() && !matches!(tool.kind, ToolKind::Slot | ToolKind::ThreadMill | ToolKind::Probe) {
        let top = ctx.stock.1.z;
        let low = pb.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(p[2]) } else { None }).fold(f64::INFINITY, f64::min);
        if low.is_finite() && top - low > tool.flute_length + 1e-6 {
            tp.warnings.push(format!("cuts {} mm deep but the flutes are only {} mm long", crate::cam::fmt_mm(top - low), crate::cam::fmt_mm(tool.flute_length)));
        }
    }
    // a probe touches where it is told: lifting it over a fixture would spoil the touch (the simulation reports any hit)
    let boxes = if op.kind == OpKind::Probe { vec![] } else { fixture_boxes(ctx.cam, ctx.part(), ctx.origin) };
    if boxes.is_empty() {
        tp.moves = pb.moves;
    } else {
        let start = DVec3::new(0.0, 0.0, pb.safe);
        // each band of the tool and holder clears the fixtures: the holder can reach them while the tip cuts beside them
        let mut moves = pb.moves;
        let mut lifts = 0;
        for (r, z0) in crate::cam_sim::tool_body(tool) {
            let lowered: Vec<_> = boxes.iter().map(|(n, lo, hi)| (n.clone(), *lo, *hi - DVec3::Z * z0)).collect();
            let (m, n) = avoid_fixtures(moves, &lowered, r, ctx.cam.fixture_lift(), start);
            moves = m;
            lifts = lifts.max(n);
        }
        tp.moves = moves;
        if lifts > 0 {
            tp.warnings.push(format!("lifts over the fixtures {lifts} time{}", if lifts == 1 { "" } else { "s" }));
        }
    }
    stats(&mut tp, tool, ctx.cam);
    tp
}

pub(crate) fn stepdown(op: &Operation, tool: &Tool) -> f64 {
    op.stepdown.unwrap_or(tool.stepdown).max(0.01)
}

pub(crate) fn stepover(op: &Operation, tool: &Tool) -> f64 {
    (op.stepover.unwrap_or(tool.stepover).clamp(0.01, 1.0)) * tool.diameter
}

fn face(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let (lo, hi) = ctx.stock;
    let top = ctx.wz(op.top).unwrap_or(hi.z);
    let bottom = ctx.wz(op.bottom).unwrap_or(ctx.part().hi.z + op.leave_floor);
    if bottom >= top - 1e-6 {
        tp.warnings.push("nothing to face: the stock top is at or below the facing height".into());
        return;
    }
    let r = t.radius();
    let step = stepover(op, t);
    // passes along X, or along Y with an angle near 90 (e.g. along the way a part slides on the face)
    let along_y = ((op.angle.rem_euclid(180.0)) - 90.0).abs() < 45.0;
    let (u0, u1, v0, v1) = if along_y { (lo.y, hi.y, lo.x, hi.x) } else { (lo.x, hi.x, lo.y, hi.y) };
    let pt = |u: f64, v: f64, z: f64| if along_y { DVec3::new(v, u, z) } else { DVec3::new(u, v, z) };
    let a0 = u0 - r - 1.0;
    let a1 = u1 + r + 1.0;
    let w = v1 - v0;
    let n = (w / step).ceil().max(1.0) as usize;
    let mut z_prev = top;
    for z in levels(top, bottom, stepdown(op, t), &[]) {
        for i in 0..=n {
            let v = v0 + w * i as f64 / n as f64;
            let (a, b) = if i % 2 == 0 { (a0, a1) } else { (a1, a0) };
            let s = pt(a, v, z);
            if i == 0 {
                pb.approach(s.x, s.y, z_prev);
                pb.cut(s, pb.plunge);
            } else {
                pb.cut(s, pb.feed);
            }
            pb.cut(pt(b, v, z), pb.feed);
        }
        z_prev = z;
    }
}

fn clear(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let (lo, hi) = ctx.stock;
    let top = rest_top(ctx, ctx.wz(op.top).unwrap_or(hi.z));
    let bottom = ctx.wz(op.bottom).unwrap_or(ctx.part().lo.z + op.leave_floor);
    let r = t.radius();
    // round the stock with room for the tool to start beside it: the passes grow in from there a stepover at
    // a time, the first of them only grazing the stock's edge
    let bound = offset(&rect(lo, hi), r + 0.5);
    let floors: Vec<f64> = ctx.geo.floors().iter().map(|f| f + op.leave_floor).collect();
    let mut z_prev = top;
    let mut any = false;
    for z in levels(top, bottom, stepdown(op, t), &floors) {
        // the floors less than leave_floor below this level stay covered
        let keep = offset(&ctx.geo.shadow(z - op.leave_floor), r + op.leave);
        let area = rest_area(ctx, difference(&bound, &keep), z, r, op.leave);
        if area.is_empty() {
            z_prev = z;
            continue;
        }
        any = true;
        let material = match &ctx.stock_in {
            Some(m) => crate::cam_3d::leftover(ctx, m, z, op.leave),
            None => difference(&rect(lo, hi), &offset(&ctx.geo.shadow(z), op.leave.max(0.0))),
        };
        pb.clear_area(&area, &material, z, z_prev, stepover(op, t), op.climb);
        z_prev = z;
    }
    if !any {
        tp.warnings.push("nothing to clear".into());
    }
}

/// Rest machining: no higher than the material that is left.
fn rest_top(ctx: &Ctx, top: f64) -> f64 {
    ctx.stock_in.as_ref().map(|m| top.min(m.max_top())).unwrap_or(top)
}

/// Rest machining: the part of a tool-centre area at height z from which the tool reaches material that is left.
fn rest_area(ctx: &Ctx, area: Shapes, z: f64, r: f64, leave: f64) -> Shapes {
    let Some(m) = &ctx.stock_in else { return area };
    let near = offset(&crate::cam_3d::leftover(ctx, m, z, leave), r + 0.5);
    let a = crate::cam_3d::intersect(&area, &near);
    // drop slivers
    offset(&offset(&a, -0.05 * r), 0.05 * r)
}

/// Closed pockets at height z that have a floor at or below z (through holes are left to hole operations).
fn pocket_area(ctx: &Ctx, op: &Operation, z: f64, grow: f64) -> Shapes {
    // the floors less than leave_floor below this level stay covered
    let zf = z - op.leave_floor;
    let mut reg = enclosed(&ctx.geo.shadow(zf));
    reg.retain(|sh| ctx.geo.floor_pts.iter().any(|f| f.z <= zf + 1e-3 && shape_inside(sh, [f.x, f.y])));
    if let Some(at) = op.at {
        let p = [at[0] + ctx.origin.x, at[1] + ctx.origin.y];
        reg.retain(|sh| shape_inside(sh, p));
    }
    offset(&reg, -grow)
}

fn pocket(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let top = rest_top(ctx, ctx.wz(op.top).unwrap_or(ctx.stock.1.z));
    let floors = ctx.geo.floors();
    let bottom = match ctx.wz(op.bottom) {
        Some(b) => b,
        None => {
            // the deepest floor that lies in a closed pocket
            let mut best = None;
            for f in floors.iter().rev() {
                if *f < top && shapes_area(&pocket_area(ctx, op, *f + op.leave_floor, 0.0)) > 1e-4 {
                    best = Some(*f);
                    break;
                }
            }
            match best {
                Some(b) => b + op.leave_floor,
                None => {
                    tp.warnings.push("no closed pockets found".into());
                    return;
                }
            }
        }
    };
    let lf: Vec<f64> = floors.iter().map(|f| f + op.leave_floor).collect();
    let mut z_prev = top;
    let mut any = false;
    let mut too_small = false;
    for z in levels(top, bottom, stepdown(op, t), &lf) {
        let raw = pocket_area(ctx, op, z, 0.0);
        if raw.is_empty() {
            // nothing cut at this level (e.g. above the part's top): the next one still starts from z_prev
            continue;
        }
        let area = offset(&raw, -(t.radius() + op.leave));
        if area.is_empty() {
            too_small = true;
            z_prev = z;
            continue;
        }
        if shapes_area(&offset(&area, t.radius() + op.leave)) < shapes_area(&raw) * 0.98 {
            too_small = true;
        }
        let area = rest_area(ctx, area, z, t.radius(), op.leave);
        if area.is_empty() {
            z_prev = z;
            any = true;
            continue;
        }
        any = true;
        let material = match &ctx.stock_in {
            Some(m) => crate::cam_3d::intersect(&raw, &crate::cam_3d::leftover(ctx, m, z, op.leave)),
            None => raw.clone(),
        };
        pb.clear_area(&area, &material, z, z_prev, stepover(op, t), op.climb);
        z_prev = z;
    }
    if too_small {
        tp.warnings.push("some pockets or corners are too narrow for this tool".into());
    }
    if !any && !too_small {
        tp.warnings.push("no closed pockets found".into());
    }
}

fn contour(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let part_bottom = ctx.part().lo.z;
    let cut_out = ctx.stock.0.z >= part_bottom - 1e-6;
    let top = ctx.wz(op.top).unwrap_or(ctx.stock.1.z);
    let bottom = ctx.wz(op.bottom).unwrap_or(if cut_out { part_bottom - op.breakthrough } else { part_bottom });
    let r = t.radius();
    let slot = t.kind == ToolKind::Slot;
    let lv = if slot { vec![bottom] } else { levels(top, bottom, stepdown(op, t), &ctx.geo.floors()) };
    let off = if slot { r - if op.slot_depth > 0.0 { op.slot_depth } else { r * 0.3 } } else { r + op.leave };
    let tab_top = bottom + op.tab_height;
    // the most a pass may take radially; extra passes outside the wall keep to it where the stock reaches further out
    let limit = crate::cam_contour::max_engagement(op, t);
    let step = stepover(op, t).min(limit);
    let seam = Seam { lead: if slot { 0.0 } else { op.lead }, overlap: op.overlap.max(0.0), at: op.seam.map(|p| DVec2::new(p[0] + ctx.origin.x, p[1] + ctx.origin.y)) };
    // cutting to size with leads: a light last lap, the one before it `finish_lap` outside the wall
    let lap = if !slot && op.lead > 0.0 && op.leave.abs() < 1e-9 { op.finish_lap.max(0.0) } else { 0.0 };
    let mut z_prev = top;
    let mut any = false;
    let mut worst: Option<(crate::cam_contour::Engagement, f64)> = None;
    let mut too_far: Option<f64> = None;
    for z in lv {
        let sh = ctx.geo.shadow(z);
        let path = offset(&sh, off);
        let first_move = pb.moves.len();
        let from = pb.pos;
        let material = if slot { vec![] } else { crate::cam_contour::material(ctx, &sh, z, op.leave) };
        let collect = |path: &Shapes| {
            let mut loops: Vec<Contour> = vec![];
            for s in path {
                for (i, c) in s.iter().enumerate() {
                    let want = match op.side {
                        Side::Outside => i == 0,
                        Side::Inside => i > 0,
                        Side::Both => true,
                    };
                    if want {
                        loops.push(if op.climb { c.iter().rev().copied().collect() } else { c.clone() });
                    }
                }
            }
            loops
        };
        let loops = collect(&path);
        if loops.is_empty() {
            // nothing cut at this level (above the part's top): the next one ramps from z_prev, not from here
            continue;
        }
        any = true;
        let tabs = (op.tabs > 0 && z < tab_top - 1e-6).then_some((op.tabs, op.tab_width, tab_top));
        // with tabs the stock round the part holds it, so it stays: no extra passes
        let mut outer_ok = false;
        if !slot && op.side != Side::Inside && op.tabs == 0 {
            let outside = difference(&material, &crate::cam_contour::filled(&sh));
            match crate::cam_contour::outer_passes(&sh, &outside, off, r, step, op.climb, 3.0 * t.diameter) {
                Ok(passes) => {
                    outer_ok = true;
                    for pass in passes {
                        outer_pass(pb, &pass, &sh, &outside, z, z_prev, r, step, seam);
                    }
                }
                Err(reach) => too_far = Some(too_far.unwrap_or(0.0).max(reach)),
            }
        }
        // what is left for the wall pass: what the extra passes didn't take
        let wall_left = if outer_ok { crate::cam_3d::intersect(&material, &offset(&sh, op.leave + step)) } else { material.clone() };
        // the laps round the wall: the light last one after one `lap` outside it, then the spring pass
        let mut laps = vec![];
        if lap > 1e-6 {
            laps.push(collect(&offset(&sh, off + lap)));
        }
        laps.push(loops);
        if op.spring_pass && !slot {
            laps.push(collect(&path));
        }
        for mut loops in laps {
        while !loops.is_empty() {
            let here = pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
            let j = (0..loops.len())
                .min_by(|a, b| {
                    let da = loops[*a].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                    let db = loops[*b].iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min);
                    da.partial_cmp(&db).unwrap()
                })
                .unwrap();
            let l = loops.remove(j);
            if slot {
                // enter from the side: plunge clear of the part, feed in, cut, feed out
                let clear_path = offset(&sh, r + 1.0);
                let s = DVec2::from(l[0]);
                let lead =
                    flat(&clear_path).iter().flat_map(|c| c.iter()).map(|q| DVec2::from(*q)).min_by(|a, b| a.distance_squared(s).partial_cmp(&b.distance_squared(s)).unwrap());
                if let Some(lead) = lead {
                    pb.approach(lead.x, lead.y, top);
                    pb.cut(DVec3::new(lead.x, lead.y, z), pb.plunge);
                    pb.cut(DVec3::new(s.x, s.y, z), pb.feed);
                    pb.cut_loop(&l, z, z, 0.01, None, None);
                    pb.cut(DVec3::new(lead.x, lead.y, z), pb.feed);
                    pb.up();
                }
            } else if !lead_loop(pb, &l, z, z_prev, &offset(&sh, r * 0.98), seam, tabs, Some((&wall_left, limit))) {
                pb.cut_loop(&l, z, z_prev, 0.05, None, tabs);
            }
        }
        }
        // how much the moves of this level really take, on the material that was there
        if !material.is_empty() {
            let cell = crate::cam_contour::check_cell(r, &material);
            if let Some(e) = crate::cam_contour::check(&pb.moves[first_move..], from, &material, z_prev, r, cell) {
                if worst.map(|w| e.peak > w.0.peak).unwrap_or(true) {
                    worst = Some((e, cell));
                }
            }
        }
        z_prev = z;
    }
    if !any {
        tp.warnings.push(match op.side {
            Side::Inside => "no inside walls to follow".into(),
            _ => "nothing to follow".into(),
        });
    }
    if let Some((e, cell)) = worst {
        if e.peak > limit + cell.max(0.01 * t.diameter) {
            let f = crate::cam::fmt_mm;
            let at = e.at - ctx.origin;
            let pct = |v: f64| format!("{:.0} % of Ø", v / t.diameter * 100.0);
            let why = match too_far {
                Some(reach) => format!(" (the stock reaches more than {} mm beyond the walls, too far for extra contour passes)", f(reach)),
                None if op.tabs > 0 => " (the tabs need the stock round the part, so there are no extra passes; allow it with a 100 % max engagement)".into(),
                None => String::new(),
            };
            tp.warnings.push(format!(
                "takes up to {} mm radially ({}) at X{} Y{} Z{}, over the {} limit{why}: rough these walls with adaptive, then finish with the contour",
                f(e.peak),
                pct(e.peak),
                f(at.x),
                f(at.y),
                f(at.z),
                pct(limit)
            ));
        }
    }
}

/// One of the extra passes outside the wall: its whole loops ramp in like the wall pass, its open pieces start
/// and end in air, so the tool comes down beside the stock and lifts off past it.
fn outer_pass(pb: &mut Pb, pass: &crate::cam_contour::Pass, sh: &Shapes, material: &Shapes, z: f64, z_from: f64, r: f64, step: f64, seam: Seam) {
    let mut segs = pass.segs.clone();
    while !segs.is_empty() {
        let here = pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
        let j = (0..segs.len())
            .min_by(|a, b| {
                let d = |k: usize| {
                    let s = &segs[k];
                    if s.closed {
                        s.pts.iter().map(|q| DVec2::from(*q).distance_squared(here)).fold(f64::INFINITY, f64::min)
                    } else {
                        DVec2::from(s.pts[0]).distance_squared(here)
                    }
                };
                d(*a).total_cmp(&d(*b))
            })
            .unwrap();
        let s = segs.remove(j);
        if s.closed {
            let left = crate::cam_3d::intersect(material, &offset(sh, pass.dist - r + step));
            if !lead_loop(pb, &s.pts, z, z_from, &offset(sh, pass.dist - 0.02 * r), seam, None, Some((&left, step))) {
                pb.cut_loop(&s.pts, z, z_from, 0.05, None, None);
            }
        } else {
            let p0 = s.pts[0];
            pb.approach(p0[0], p0[1], z_from);
            pb.cut(DVec3::new(p0[0], p0[1], z), pb.plunge);
            for p in &s.pts[1..] {
                pb.cut(DVec3::new(p[0], p[1], z), pb.feed);
            }
        }
    }
}

/// How a closed contour pass starts and ends (see `lead_loop`).
#[derive(Clone, Copy)]
struct Seam {
    /// Arc radius tried first; smaller ones are tried where it doesn't fit.
    lead: f64,
    /// Run on past the start this far before leading off.
    overlap: f64,
    /// Start on the side nearest this point; None: the middle of the longest side.
    at: Option<DVec2>,
}

/// Cut a closed loop at `z` with arcs onto and off it, on the side away from the material (`keep_out`: where the
/// tool's centre mustn't go), starting mid-way along a side: the longest straight one (or the one nearest
/// `seam.at`), so the seam is away from corners and arcs. The tool comes down along the far half of the lead-in,
/// clear of the wall; the pass runs `seam.overlap` past its start so the end blends over it, and the lead-out
/// rises a little so the tool isn't rubbing the wall at depth as it unloads. The arc radius shrinks (down to a
/// quarter of the tool's radius) where `seam.lead` doesn't fit. False when nothing fits.
/// `guard`: the material there and the most the tool may take radially; a lead-in that would take more (arcing
/// into stock left on the wall) is passed over for another start, and with none left the caller ramps instead.
fn lead_loop(pb: &mut Pb, l: &[P2], z: f64, z_from: f64, keep_out: &Shapes, seam: Seam, tabs: Option<(u32, f64, f64)>, guard: Option<(&Shapes, f64)>) -> bool {
    let n = l.len();
    if n < 3 || seam.lead <= 0.0 {
        return false;
    }
    let side_len = |i: usize| DVec2::from(l[i]).distance(DVec2::from(l[(i + 1) % n]));
    let mid = |i: usize| (DVec2::from(l[i]) + DVec2::from(l[(i + 1) % n])) / 2.0;
    let mut starts: Vec<usize> = (0..n).filter(|i| side_len(*i) > 1e-3).collect();
    match seam.at {
        Some(p) => starts.sort_by(|a, b| mid(*a).distance_squared(p).total_cmp(&mid(*b).distance_squared(p))),
        // the longest straight sides first; ties (a regular polygon, an arc's chords) nearest the tool
        None => {
            let here = pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
            starts.sort_by(|a, b| (side_len(*b) * 100.0).round().total_cmp(&(side_len(*a) * 100.0).round()).then(mid(*a).distance_squared(here).total_cmp(&mid(*b).distance_squared(here))));
        }
    }
    let r = pb.tool.radius();
    let steps = 12;
    let mut radius = seam.lead;
    while radius >= 0.25 * r - 1e-9 {
        for &i in starts.iter().take(24) {
            let s0 = mid(i);
            let t = (DVec2::from(l[(i + 1) % n]) - DVec2::from(l[i])).normalize();
            // run on along the same side only (with tabs the loop is broken up anyway)
            let over = if tabs.is_some() { 0.0 } else { seam.overlap.clamp(0.0, (side_len(i) / 2.0 - 1e-3).max(0.0)) };
            let s1 = s0 + t * over;
            for side in [1.0, -1.0] {
                // centre beside the start; in along a quarter circle ending tangent to the loop, out along another
                let u = -t.perp() * side;
                let (c_in, c_out) = (s0 - u * radius, s1 - u * radius);
                let quarter = |f: &dyn Fn(f64) -> DVec2| (0..=steps).map(|k| f(k as f64 / steps as f64 * std::f64::consts::FRAC_PI_2)).collect::<Vec<DVec2>>();
                let lead_in = quarter(&|a: f64| c_in + (u * a.sin() - t * a.cos()) * radius);
                let lead_out = quarter(&|a: f64| c_out + (u * a.cos() + t * a.sin()) * radius);
                if lead_in.iter().chain(&lead_out).any(|p| inside(keep_out, [p.x, p.y])) {
                    continue;
                }
                if let Some((material, limit)) = guard {
                    let moves: Vec<Move> = lead_in.iter().skip(1).map(|p| Move::Feed(p.extend(z).to_array(), pb.feed)).collect();
                    let (lo, hi) = lead_in.iter().fold((DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY)), |(a, b), p| (a.min(*p), b.max(*p)));
                    let near = crate::cam_3d::intersect(material, &rect((lo - DVec2::splat(r + 1.0)).extend(0.0), (hi + DVec2::splat(r + 1.0)).extend(0.0)));
                    let cell = (r / 16.0).max(0.02);
                    let e = crate::cam_contour::check(&moves, Some(lead_in[0].extend(z)), &near, z + 1.0, r, cell);
                    if e.map(|e| e.peak > limit).unwrap_or(false) {
                        continue;
                    }
                }
                // the loop from the middle of that side
                let mut lp: Vec<P2> = vec![[s0.x, s0.y]];
                lp.extend((1..=n).map(|k| l[(i + k) % n]));
                pb.approach(lead_in[0].x, lead_in[0].y, z_from);
                let dz = z_from - z;
                if dz > 1e-6 {
                    // ramp down back and forth along the far half of the lead-in, clear of the wall: never a Z move
                    // with the tool against it (and never a plunge into the stock)
                    pb.cut(lead_in[0].extend(z_from), pb.plunge);
                    let far = &lead_in[..=steps / 2];
                    let len: f64 = far.windows(2).map(|w| w[0].distance(w[1])).sum();
                    let per_pass = 2.0 * len * pb.ramp.tan().max(1e-3);
                    let pairs = ((dz / per_pass).ceil() as usize).clamp(1, 1000);
                    let step = dz / (2 * pairs) as f64;
                    let mut zz = z_from;
                    for _ in 0..pairs {
                        for path in [far.iter().collect::<Vec<_>>(), far.iter().rev().collect::<Vec<_>>()] {
                            let z0 = zz;
                            let mut d = 0.0;
                            for w in path.windows(2) {
                                d += w[0].distance(*w[1]);
                                pb.cut(w[1].extend(z0 - step * d / len.max(1e-9)), pb.feed);
                            }
                            zz = z0 - step;
                        }
                    }
                }
                pb.cut(lead_in[0].extend(z), pb.plunge);
                for p in &lead_in[1..] {
                    pb.cut(p.extend(z), pb.feed);
                }
                pb.cut_loop(&lp, z, z, 0.05, None, tabs);
                if over > 0.0 {
                    pb.cut(s1.extend(z), pb.feed);
                }
                // off the wall rising a little (at most to where the pass came down from)
                let lift = (0.25 * radius).min(1.0).min(dz.max(0.0));
                for (k, p) in lead_out[1..].iter().enumerate() {
                    pb.cut(p.extend(z + lift * (k + 1) as f64 / steps as f64), pb.feed);
                }
                return true;
            }
        }
        radius *= 0.7;
    }
    false
}

/// How far a chamfer mill's cone may reach past the chamfer it cuts into a face beside the edge, mm.
pub const CHAMFER_GOUGE: f64 = 0.05;

fn chamfer(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let edge = ctx.wz(op.top).unwrap_or(ctx.part().hi.z);
    let half = (t.angle.max(10.0) / 2.0).to_radians();
    let tip = if t.kind == ToolKind::Chamfer { t.tip_diameter / 2.0 } else { 0.0 };
    let w = op.chamfer.max(0.0);
    // How far the tip goes below the chamfer: 1 mm by default, so it cuts with the cone above its weak, slow
    // point; less if the cone isn't wide enough for that (at least 0.3 mm).
    let room = (t.radius() - tip) / half.tan() - w / half.tan();
    let want = if op.tip_offset > 0.0 { op.tip_offset } else { 1.0 };
    let e = want.min(room).max(0.3_f64.min(want));
    if op.tip_offset > 0.0 && e < want - 1e-6 {
        tp.warnings.push(format!("the tip offset is cut to {} mm: the chamfer mill's cone isn't wide enough for more", crate::cam::fmt_mm(e)));
    }
    let depth = w / half.tan() + e;
    let s = tip + e * half.tan();
    if tip + depth * half.tan() > t.radius() + 1e-6 {
        tp.warnings.push(format!("a {} mm chamfer is too wide for this tool", crate::cam::fmt_mm(w)));
        return;
    }
    let z = edge - depth;
    let sh = ctx.geo.shadow(edge - 0.01);
    let path = offset(&sh, s + op.leave);
    let mut any = false;
    // The whole cone, tip offset and all, against the part along each edge: past a vertical (or steeper than
    // the cone) face it touches only the edge, cutting the chamfer; next to a face shallower than the cone
    // (a dish's rim, a gentle slope) the cone below the edge cuts into that face all along it. Such edges are
    // left out (a ball edge break is the tool for them).
    let cone = crate::cam_3d::Dropper::new(ctx.part(), t, 0.0);
    let mut flat = t.clone();
    flat.kind = ToolKind::Flat;
    flat.diameter = (2.0 * tip).max(0.2);
    flat.corner_radius = 0.0;
    let under = crate::cam_3d::Dropper::new(ctx.part(), &flat, 0.0);
    let mut scratch = crate::cam_3d::Scratch::default();
    let chamfer_z = w / half.tan();
    let diag = std::env::var_os("RCAD_CHAMFER_DIAG").is_some();
    // the most the cone cuts past the chamfer, and the tip's clearance over what is under it, along a loop
    let mut check = |l: &Contour| -> (f64, f64, DVec2) {
        let (mut worst, mut clear, mut at) = (f64::NEG_INFINITY, f64::INFINITY, DVec2::from(l[0]));
        let n = l.len();
        for i in 0..n {
            let (a, b) = (DVec2::from(l[i]), DVec2::from(l[(i + 1) % n]));
            let steps = (a.distance(b) / 0.25).ceil().max(1.0) as usize;
            for k in 0..steps {
                let q = a.lerp(b, k as f64 / steps as f64);
                // past the chamfer: the cone resting higher than the chamfer's depth, or the tip's flat in the part
                let (h, _) = cone.drop(q, &mut scratch);
                let (u, _) = under.drop(q, &mut scratch);
                let bad = (h - z - chamfer_z).max(u - z);
                if bad.is_finite() && bad > worst {
                    worst = bad;
                    at = q;
                }
                if u.is_finite() {
                    clear = clear.min(z - u);
                }
            }
        }
        (worst, clear, at)
    };
    for sp in &path {
        for (i, c) in sp.iter().enumerate() {
            let want = match op.side {
                Side::Outside => i == 0,
                Side::Inside => i > 0,
                Side::Both => true,
            };
            if want {
                let l: Contour = if op.climb { c.iter().rev().copied().collect() } else { c.clone() };
                if l.len() < 2 {
                    continue;
                }
                let (cut, clear, at) = check(&l);
                if diag {
                    eprintln!("chamfer {}: loop of {} points near ({:.2}, {:.2}): cone past the chamfer {:.3} mm at ({:.2}, {:.2}), tip {:.2} mm over what is under it", op.name, l.len(), l[0][0] - ctx.origin.x, l[0][1] - ctx.origin.y, cut, at.x - ctx.origin.x, at.y - ctx.origin.y, clear);
                }
                                if cut > CHAMFER_GOUGE {
                    tp.warnings.push(format!(
                        "left out the edge near X{} Y{}: the face beside it is shallower than the chamfer mill's cone, which would cut {} mm into it; break that edge with a ball instead",
                        crate::cam::fmt_mm(at.x - ctx.origin.x), crate::cam::fmt_mm(at.y - ctx.origin.y), crate::cam::fmt_mm(cut)
                    ));
                    continue;
                }
                // come down above the edge, not to the chamfer's bottom: a cone lowered to z + retract would
                // already reach into the edge when the chamfer is deeper than the retract
                let here = pb.pos.map(|p| p.truncate());
                let k = here.map(|h| (0..l.len()).min_by(|a, b| DVec2::from(l[*a]).distance_squared(h).total_cmp(&DVec2::from(l[*b]).distance_squared(h))).unwrap()).unwrap_or(0);
                let s0 = l[k];
                pb.approach(s0[0], s0[1], edge.max(z));
                pb.cut(DVec3::new(s0[0], s0[1], z), pb.plunge);
                pb.cut_loop(&l, z, z, 0.01, None, None);
                any = true;
            }
        }
    }
    if !any {
        tp.warnings.push("no edges found at that height".into());
    }
}

fn thread_spec(d: f64) -> Option<(f64, f64)> {
    METRIC_COARSE.iter().find(|(m, p)| (d - m).abs() < 0.1 || (d - (m - p)).abs() < 0.2).copied()
}

fn holes_op(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let mut holes: Vec<Hole> = if op.points.is_empty() {
        ctx.geo.holes()
    } else {
        let top = ctx.wz(op.top).unwrap_or(ctx.part().hi.z);
        let Some(bottom) = ctx.wz(op.bottom) else {
            tp.warnings.push("give a bottom height for the points".into());
            return;
        };
        let r = op.thread_diameter.map(|d| d / 2.0).unwrap_or(t.radius());
        op.points.iter().map(|p| Hole { x: p[0] + ctx.origin.x, y: p[1] + ctx.origin.y, r, top, bottom, through: false }).collect()
    };
    let d = t.diameter;
    let (dmin, dmax) = match op.kind {
        OpKind::Drill => match t.kind {
            ToolKind::SpotDrill | ToolKind::CenterDrill => (0.0, f64::INFINITY),
            _ => (d - 0.05, d + (d * 0.1).max(0.5)),
        },
        OpKind::Bore => (d * 1.05, f64::INFINITY),
        OpKind::Tap => (d - 1.3 * t.pitch, d + 0.1),
        OpKind::Thread => match op.thread_diameter {
            Some(m) => (m - 1.6 * op.pitch.unwrap_or(t.pitch.max(0.5)), m + 0.1),
            None => (d * 1.05, f64::INFINITY),
        },
        _ => (0.0, f64::INFINITY),
    };
    let (dmin, dmax) = (op.hole_min.unwrap_or(dmin), op.hole_max.unwrap_or(dmax));
    if op.points.is_empty() {
        holes.retain(|h| h.r * 2.0 >= dmin - 1e-6 && h.r * 2.0 <= dmax + 1e-6);
    }
    if let Some(at) = op.at {
        let p = DVec2::new(at[0] + ctx.origin.x, at[1] + ctx.origin.y);
        holes.retain(|h| DVec2::new(h.x, h.y).distance(p) <= h.r + 0.01);
    }
    if holes.is_empty() {
        tp.warnings.push(format!(
            "no holes between Ø{} and Ø{} for this tool",
            crate::cam::fmt_mm(dmin.max(0.0)),
            if dmax.is_finite() { crate::cam::fmt_mm(dmax) } else { "∞".into() }
        ));
        return;
    }
    // nearest-neighbour order
    let mut order = vec![];
    let mut here = DVec2::new(ctx.stock.0.x, ctx.stock.0.y);
    while !holes.is_empty() {
        let j =
            (0..holes.len()).min_by(|a, b| DVec2::new(holes[*a].x, holes[*a].y).distance(here).partial_cmp(&DVec2::new(holes[*b].x, holes[*b].y).distance(here)).unwrap()).unwrap();
        let h = holes.remove(j);
        here = DVec2::new(h.x, h.y);
        order.push(h);
    }
    let mut skipped = 0;
    for h in order {
        let top = ctx.wz(op.top).filter(|_| op.points.is_empty()).unwrap_or(h.top);
        let through_extra = if h.through { op.breakthrough } else { 0.0 };
        // come down over the stock, not just over the hole's top: material above it (a pocket not cut yet,
        // the stock's top margin) would be in the way of the rapid
        let entry = top.max(ctx.stock.1.z);
        match op.kind {
            OpKind::Drill => {
                let bottom = match ctx.wz(op.bottom).filter(|_| op.points.is_empty()) {
                    Some(b) => b,
                    None => match t.kind {
                        ToolKind::SpotDrill | ToolKind::CenterDrill => {
                            let spot_d = (h.r * 2.0 + 2.0 * op.chamfer).min(t.diameter);
                            top - (spot_d / 2.0) / (t.angle.max(10.0) / 2.0).to_radians().tan()
                        }
                        ToolKind::Drill if h.through => h.bottom - through_extra - t.tip_length(),
                        _ => h.bottom - through_extra,
                    },
                };
                let rz = top + pb.retract;
                pb.approach(h.x, h.y, entry);
                if op.peck > 0.0 {
                    let mut cur = top;
                    while cur > bottom + 1e-6 {
                        let next = (cur - op.peck).max(bottom);
                        if cur < top - 1e-6 {
                            pb.rapid(DVec3::new(h.x, h.y, cur + 0.3));
                        }
                        pb.cut(DVec3::new(h.x, h.y, next), pb.plunge);
                        cur = next;
                        if cur > bottom + 1e-6 {
                            pb.rapid(DVec3::new(h.x, h.y, rz));
                        }
                    }
                } else {
                    pb.cut(DVec3::new(h.x, h.y, bottom), pb.plunge);
                }
                if op.dwell > 0.0 {
                    pb.moves.push(Move::Dwell(op.dwell));
                }
                pb.rapid(DVec3::new(h.x, h.y, rz));
            }
            OpKind::Bore => {
                let rh = h.r - t.radius() - op.leave;
                if rh <= 0.01 {
                    skipped += 1;
                    continue;
                }
                let bottom = ctx.wz(op.bottom).filter(|_| op.points.is_empty()).unwrap_or(h.bottom - through_extra);
                let pitch = stepdown(op, t).min(2.0 * std::f64::consts::PI * rh * pb.ramp.tan()).max(0.05);
                let dir = if op.climb { 1.0 } else { -1.0 };
                pb.approach(h.x + rh, h.y, entry);
                pb.cut(DVec3::new(h.x + rh, h.y, top), pb.plunge);
                helix(pb, h.x, h.y, rh, top, bottom, -pitch, dir);
                helix(pb, h.x, h.y, rh, bottom, bottom, 0.0, dir);
                pb.cut(DVec3::new(h.x, h.y, bottom), pb.feed);
                pb.rapid(DVec3::new(h.x, h.y, top + pb.retract));
            }
            OpKind::Thread => {
                let (major, pitch) = match (op.thread_diameter, op.pitch) {
                    (Some(m), Some(p)) => (m, p),
                    (Some(m), None) => (m, if t.pitch > 0.0 { t.pitch } else { thread_spec(m).map(|s| s.1).unwrap_or(1.0) }),
                    (None, p) => match thread_spec(h.r * 2.0) {
                        Some((m, sp)) => (m, p.unwrap_or(if t.pitch > 0.0 { t.pitch } else { sp })),
                        None => {
                            skipped += 1;
                            continue;
                        }
                    },
                };
                if t.diameter >= h.r * 2.0 - 0.05 {
                    skipped += 1;
                    continue;
                }
                let rh = major / 2.0 - t.radius();
                if rh <= 0.01 {
                    skipped += 1;
                    continue;
                }
                let bottom = ctx.wz(op.bottom).filter(|_| op.points.is_empty()).unwrap_or(if h.through { h.bottom - through_extra } else { h.bottom + pitch });
                let length = (top - bottom).max(0.0);
                let revs = if t.cutting_width >= length + pitch { 1.0 } else { (length / pitch).ceil() + 1.0 };
                // right-hand internal, climbing: counter-clockwise going up
                let dir = if op.left_hand { -1.0 } else { 1.0 };
                pb.approach(h.x, h.y, entry);
                pb.cut(DVec3::new(h.x, h.y, bottom), pb.plunge);
                pb.cut(DVec3::new(h.x + rh, h.y, bottom), pb.feed * 0.5);
                helix(pb, h.x, h.y, rh, bottom, bottom + pitch * revs, pitch, dir);
                let p = pb.pos.unwrap();
                pb.cut(DVec3::new(h.x, h.y, p.z), pb.feed * 0.5);
                pb.rapid(DVec3::new(h.x, h.y, top + pb.retract));
            }
            OpKind::Tap => {
                let bottom = ctx.wz(op.bottom).filter(|_| op.points.is_empty()).unwrap_or(if h.through { h.bottom - through_extra } else { h.bottom + t.pitch * 2.0 });
                let rz = top + pb.retract;
                pb.approach(h.x, h.y, entry);
                pb.moves.push(Move::Tap { z: bottom, r: rz, pitch: t.pitch });
                pb.pos = Some(DVec3::new(h.x, h.y, rz));
            }
            _ => {}
        }
    }
    if skipped > 0 {
        tp.warnings.push(format!("{skipped} hole(s) skipped: too small for this tool or no thread size"));
    }
}

/// Helix around (cx, cy) at radius r from z0 to z1 (pitch per turn, sign gives direction of travel;
/// pitch 0 makes one flat circle). dir 1 = counter-clockwise.
fn helix(pb: &mut Pb, cx: f64, cy: f64, r: f64, z0: f64, z1: f64, pitch: f64, dir: f64) {
    let turns = if pitch.abs() < 1e-9 { 1.0 } else { ((z1 - z0) / pitch).abs() };
    let seg = (2.0 * (1.0 - (0.005 / r).min(1.0)).acos()).clamp(0.05, 0.5);
    let n = ((turns * std::f64::consts::TAU / seg).ceil() as usize).max(8);
    for i in 1..=n {
        let f = i as f64 / n as f64;
        let a = dir * f * turns * std::f64::consts::TAU;
        let z = if pitch.abs() < 1e-9 { z0 } else { z0 + (z1 - z0) * f };
        pb.cut(DVec3::new(cx + r * a.cos(), cy + r * a.sin(), z), pb.feed);
    }
}

/// The program's run time: each operation's moves, the spin-up wait at each spindle start (where the post
/// writes one) and the rapids from one operation to the next (each toolpath's own time starts at its first move).
pub fn cycle_time(cam: &Cam, paths: &[Toolpath]) -> f64 {
    let rapid_rate = cam.rapid_rate.max(1.0);
    let rapid_z = if cam.machine.rapid_z > 0.0 { cam.machine.rapid_z } else { rapid_rate };
    let mut total = 0.0;
    let (mut tool, mut rpm, mut end): (Option<u64>, Option<f64>, Option<DVec3>) = (None, None, None);
    for tp in paths.iter().filter(|p| !p.moves.is_empty()) {
        // the post leaves out operations without a tool
        let Some(t) = cam.path_tool(tp.op, tp.tool) else { continue };
        let t: &Tool = &t;
        total += tp.time;
        if tool != Some(t.id) {
            tool = Some(t.id);
            rpm = None;
        }
        let r = cam.spindle_rpm(t).round();
        if rpm != Some(r) && t.kind != ToolKind::Probe {
            rpm = Some(r);
            total += cam.machine.spin_up.max(0.0);
        }
        let start = tp.moves[0].end(DVec3::ZERO);
        if let Some(e) = end {
            total += ((start - e).truncate().length() / rapid_rate).max((start.z - e.z).abs() / rapid_z) * 60.0;
        }
        end = Some(tp.moves.iter().fold(DVec3::ZERO, |p, m| m.end(p)));
    }
    total
}

/// The cuts of one toolpath in an order that finishes one region before going to the next: each cut (its moves
/// down to the work and along it) keeps every earlier cut whose reach (its extent plus the tool's diameter)
/// overlaps its own before it, so depth levels and an area's own order stay as they were; among the cuts free to
/// go next, the one starting nearest to where the tool is goes. Cut directions are kept (a climb stays a climb).
///
/// A cut ends where the tool lifts: the rapids up to the top of a lift belong to the cut before (its exit), the
/// rest of them to the next (its approach). Rapids that stay at depth (an adaptive path moving through what it
/// cleared) don't end a cut. A cut that no longer follows the one it followed goes there over the path's
/// clearance height, so the program stays safe as it is; `tighten_links` then lowers those links.
pub fn order_by_region(moves: &[Move], r: f64) -> Vec<Move> {
    let n = moves.len();
    let mut ends = vec![DVec3::ZERO; n];
    let mut p = DVec3::ZERO;
    for (i, m) in moves.iter().enumerate() {
        p = m.end(p);
        ends[i] = p;
    }
    let clear = ends.iter().map(|e| e.z).fold(f64::NEG_INFINITY, f64::max);
    // where each cut starts: after the top of each lift that comes after some cutting
    let mut starts = vec![0usize];
    let mut cut_seen = false;
    let mut i = 0;
    while i < n {
        if !matches!(moves[i], Move::Rapid(_)) {
            cut_seen = true;
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && matches!(moves[j], Move::Rapid(_)) {
            j += 1;
        }
        let z0 = if i > 0 { ends[i - 1].z } else { f64::NEG_INFINITY };
        let top = (i..j).map(|k| ends[k].z).fold(f64::NEG_INFINITY, f64::max);
        if cut_seen && j < n && top > z0 + 1e-6 {
            let k = (i..j).find(|&k| ends[k].z >= top - 1e-9).unwrap();
            starts.push(k + 1);
            cut_seen = false;
        }
        i = j;
    }
    // trailing moves after the last cutting (the final retract) stay last
    let last_cut = (0..n).rev().find(|&k| !matches!(moves[k], Move::Rapid(_)));
    let Some(last_cut) = last_cut else { return moves.to_vec() };
    let tail = last_cut + 1;
    let cuts: Vec<(usize, usize)> = starts.iter().enumerate().map(|(c, &s)| (s, starts.get(c + 1).copied().unwrap_or(tail).min(tail))).filter(|c| c.0 < c.1).collect();
    if cuts.len() < 3 {
        return moves.to_vec();
    }
    // each cut's start (where its approach ends), end, and reach box in XY (its cutting moves)
    let info: Vec<(DVec3, DVec3, DVec2, DVec2)> = cuts
        .iter()
        .map(|&(s, e)| {
            let mut lo = DVec2::splat(f64::INFINITY);
            let mut hi = DVec2::splat(f64::NEG_INFINITY);
            let mut start = None;
            for k in s..e {
                if !matches!(moves[k], Move::Rapid(_)) {
                    let from = if k > 0 { ends[k - 1] } else { ends[k] };
                    start.get_or_insert(from);
                    for q in [from, ends[k]] {
                        lo = lo.min(q.truncate());
                        hi = hi.max(q.truncate());
                    }
                }
            }
            let start = start.unwrap_or(ends[s]);
            if lo.x > hi.x {
                lo = start.truncate();
                hi = lo;
            }
            (start, ends[e - 1], lo - DVec2::splat(2.0 * r), hi + DVec2::splat(2.0 * r))
        })
        .collect();
    let overlap = |a: usize, b: usize| {
        let (_, _, alo, ahi) = info[a];
        let (_, _, blo, bhi) = info[b];
        alo.x <= bhi.x && blo.x <= ahi.x && alo.y <= bhi.y && blo.y <= ahi.y
    };
    let m = cuts.len();
    let before: Vec<Vec<usize>> = (0..m).map(|b| (0..b).filter(|&a| overlap(a, b)).collect()).collect();
    let mut done = vec![false; m];
    let mut order = vec![0usize];
    done[0] = true;
    let mut at = info[0].1;
    for _ in 1..m {
        let next = (0..m)
            .filter(|&c| !done[c] && before[c].iter().all(|&a| done[a]))
            .min_by(|&a, &b| info[a].0.truncate().distance(at.truncate()).total_cmp(&info[b].0.truncate().distance(at.truncate())).then(a.cmp(&b)))
            .unwrap();
        done[next] = true;
        order.push(next);
        at = info[next].1;
    }
    let mut out = Vec::with_capacity(n + 2 * m);
    let mut prev: Option<usize> = None;
    for c in order {
        let (s, e) = cuts[c];
        if let Some(pc) = prev {
            if pc + 1 != c {
                // a new neighbour: up to the clearance height here, across, then the cut's own approach
                let here = ends[cuts[pc].1 - 1];
                let first = ends[s];
                if here.z < clear - 1e-9 {
                    out.push(Move::Rapid([here.x, here.y, clear]));
                }
                out.push(Move::Rapid([first.x, first.y, clear]));
            }
        }
        out.extend_from_slice(&moves[s..e]);
        prev = Some(c);
    }
    if let Some(pc) = prev {
        if pc + 1 != m && tail < n {
            let here = ends[cuts[pc].1 - 1];
            if here.z < clear - 1e-9 {
                out.push(Move::Rapid([here.x, here.y, clear]));
            }
        }
    }
    out.extend_from_slice(&moves[tail..]);
    out
}

/// A ramp down starting at `from` (the moves from `i` on): feed moves staying within `reach` of it that come
/// back over it lower down, the next move going on at that height. The index of the move that comes back,
/// its height, and the ramp's feed.
fn ramp_back_to(moves: &[Move], i: usize, from: DVec3, reach: f64) -> Option<(usize, f64, f64)> {
    let mut found = None;
    let mut f_ramp = f64::INFINITY;
    for (m, mv) in moves.iter().enumerate().skip(i) {
        let Move::Feed(q, f) = *mv else { break };
        let q = DVec3::from(q);
        if q.truncate().distance(from.truncate()) > reach || q.z > from.z + 1e-6 {
            break;
        }
        f_ramp = f_ramp.min(f);
        if q.truncate().distance(from.truncate()) < 1e-6 && q.z < from.z - 1e-6 {
            found = Some((m, q.z));
        }
    }
    let (m, z) = found?;
    // what follows runs on at that height (the lead-in), or the ramp just ends there
    match moves.get(m + 1) {
        Some(Move::Feed(q, _)) if (q[2] - z).abs() > 1e-6 => None,
        _ => Some((m, z, f_ramp)),
    }
}

/// Margin the tightened links keep over the stock still there, the part and the fixtures, mm.
pub const LINK_MARGIN: f64 = 1.0;

/// Shorten the moves between cuts (`Cam::stay_down`): with the stock simulated up to each link, the tool travels
/// to the next cut only as high as the stock still left along the way needs (its highest point under the tool,
/// with the fixtures, plus `LINK_MARGIN`), so through cleared space or past the stock's end it stays down at its
/// cutting depth; and it rapids down to `LINK_MARGIN` over the stock under the next entry, feeding only the rest.
/// Links between operations count too, where they keep the tool, its speed and the coolant. Links that don't
/// (a tool change, a new speed) keep the full retract. The material each cut removes is the same.
pub fn tighten_links(cam: &Cam, part: &Part, stock_in: Option<&StockMap>, mut paths: Vec<Toolpath>) -> Vec<Toolpath> {
    for tp in paths.iter_mut() {
        if tp.probe.is_none() {
            let r = cam.tool(tp.tool).map(|t| t.diameter / 2.0).unwrap_or(3.0);
            tp.moves = order_by_region(&tp.moves, r);
        }
    }
    let stock = stock_box(cam, part);
    let mut sim = crate::cam_sim::Sim::new(cam, part, stock, &paths, stock_resolution(stock));
    if let Some(m) = stock_in {
        sim.start_from(m);
    }
    let fixtures: Vec<(DVec3, DVec3)> = fixture_boxes(cam, part, work_origin(cam, part)).into_iter().map(|f| (f.1, f.2)).collect();
    let fix_clear = if cam.fixture_clearance > 0.0 { cam.fixture_clearance } else { cam.retract.max(1.0) };
    // the highest stock (or fixture) within `r` of (x, y)
    let high = |sim: &crate::cam_sim::Sim, x: f64, y: f64, r: f64| -> f64 {
        let mut h = f64::NEG_INFINITY;
        let reach = r + 0.5 + sim.cell;
        let (i0, i1) = (((x - reach - sim.x0) / sim.cell).floor().max(0.0) as usize, ((x + reach - sim.x0) / sim.cell).ceil().max(0.0) as usize);
        let (j0, j1) = (((y - reach - sim.y0) / sim.cell).floor().max(0.0) as usize, ((y + reach - sim.y0) / sim.cell).ceil().max(0.0) as usize);
        for j in j0..=j1.min(sim.ny.saturating_sub(1)) {
            for i in i0..=i1.min(sim.nx.saturating_sub(1)) {
                let (cx, cy) = (sim.x0 + i as f64 * sim.cell, sim.y0 + j as f64 * sim.cell);
                if (cx - x).hypot(cy - y) <= reach {
                    let v = sim.stock[j * sim.nx + i];
                    if v != crate::cam_stock::EMPTY {
                        h = h.max(v as f64);
                    }
                }
            }
        }
        for (lo, hi) in &fixtures {
            let dx = (lo.x - x).max(x - hi.x).max(0.0);
            let dy = (lo.y - y).max(y - hi.y).max(0.0);
            if dx.hypot(dy) < reach {
                h = h.max(hi.z + fix_clear - LINK_MARGIN);
            }
        }
        h
    };
    // what each path runs with: (tool, rpm, coolant)
    let setting = |tp: &Toolpath| -> Option<(u64, i64, bool)> {
        let tool = cam.path_tool(tp.op, tp.tool)?;
        let op = cam.op(tp.op);
        Some((tp.tool, cam.spindle_rpm(&tool).round() as i64, cam.coolant_for(&tool, op).is_on()))
    };
    let mut base = 0usize;
    let mut prev: Option<(usize, DVec3)> = None; // the path before and where its last cut ended
    for k in 0..paths.len() {
        let n = paths[k].moves.len();
        if paths[k].probe.is_some() || n == 0 {
            sim.run_to(base + n, usize::MAX);
            base += n;
            prev = None;
            continue;
        }
        let r = cam.tool(paths[k].tool).map(|t| t.diameter / 2.0).unwrap_or(3.0);
        let mut out: Vec<Move> = Vec::with_capacity(n);
        let mut pos: Option<DVec3> = None;
        let mut i = 0usize;
        // a link into this path from the one before (same tool, speed and coolant): its trailing rapids go
        let mut from = match prev {
            Some((pk, a)) if setting(&paths[pk]) == setting(&paths[k]) && setting(&paths[k]).is_some() && matches!(paths[k].moves.first(), Some(Move::Rapid(_))) => {
                let pm = &mut paths[pk].moves;
                while matches!(pm.last(), Some(Move::Rapid(_))) {
                    pm.pop();
                }
                paths[k].linked = true;
                Some(a)
            }
            _ => None,
        };
        while i < n {
            let m = paths[k].moves[i];
            if let Move::Rapid(_) = m {
                // a run of rapids, and the vertical plunge after it
                let mut j = i;
                while j < n && matches!(paths[k].moves[j], Move::Rapid(_)) {
                    j += 1;
                }
                let b = paths[k].moves[j - 1].end(DVec3::ZERO);
                let plunge = match paths[k].moves.get(j) {
                    Some(Move::Feed(q, f)) if (q[0] - b.x).abs() < 1e-6 && (q[1] - b.y).abs() < 1e-6 && q[2] < b.z => Some((q[2], *f)),
                    _ => None,
                };
                let a = from.or(pos);
                match (a, j < n) {
                    (Some(a), true) => {
                        sim.run_to(base + i, usize::MAX);
                        // the highest thing along the way, under the tool
                        let d = (b - a).truncate().length();
                        let steps = (d / (sim.cell.min(r / 2.0)).max(0.05)).ceil().max(1.0) as usize;
                        let mut h_path = f64::NEG_INFINITY;
                        for s in 0..=steps {
                            let q = a.truncate().lerp(b.truncate(), s as f64 / steps as f64);
                            h_path = h_path.max(high(&sim, q.x, q.y, r));
                        }
                        let travel = (h_path + LINK_MARGIN).max(a.z);
                        if travel > a.z + 1e-6 {
                            out.push(Move::Rapid([a.x, a.y, travel]));
                        }
                        out.push(Move::Rapid([b.x, b.y, travel]));
                        match plunge {
                            Some((z, f)) => {
                                let h_b = high(&sim, b.x, b.y, r);
                                let down = (h_b + LINK_MARGIN).max(z).min(travel);
                                if down < travel - 1e-6 {
                                    out.push(Move::Rapid([b.x, b.y, down]));
                                }
                                if down > z + 1e-6 {
                                    out.push(Move::Feed([b.x, b.y, z], f));
                                }
                                pos = Some(DVec3::new(b.x, b.y, z));
                                i = j + 1;
                                // a ramp down at the entry (a contour's lead-in rocking back and forth, a helix)
                                // that comes back over the entry point lower down: where the stock simulated up
                                // to here leaves that column clear, go straight down instead
                                if let Some((m, z_end, f_ramp)) = ramp_back_to(&paths[k].moves, i, DVec3::new(b.x, b.y, z), 2.0 * r + 2.0) {
                                    sim.run_to(base + i, usize::MAX);
                                    let h = high(&sim, b.x, b.y, r);
                                    if h < z_end + 0.02 {
                                        let down = (h + LINK_MARGIN).max(z_end);
                                        if down < z - 1e-6 {
                                            out.push(Move::Rapid([b.x, b.y, down]));
                                        }
                                        if down > z_end + 1e-6 {
                                            out.push(Move::Feed([b.x, b.y, z_end], f.min(f_ramp)));
                                        }
                                        pos = Some(DVec3::new(b.x, b.y, z_end));
                                        i = m + 1;
                                    }
                                }
                            }
                            None => {
                                // down to the entry: rapid only to over the stock there, feed the rest
                                let h_b = high(&sim, b.x, b.y, r);
                                let down = (h_b + LINK_MARGIN).max(b.z).min(travel);
                                if down < travel - 1e-6 {
                                    out.push(Move::Rapid([b.x, b.y, down]));
                                }
                                if down > b.z + 1e-6 {
                                    let f = match paths[k].moves.get(j) {
                                        Some(Move::Feed(_, f)) => *f,
                                        _ => cam.path_tool(paths[k].op, paths[k].tool).map(|t| t.plunge_feed).unwrap_or(300.0),
                                    };
                                    out.push(Move::Feed([b.x, b.y, b.z], f));
                                }
                                pos = Some(b);
                                i = j;
                            }
                        }
                    }
                    _ => {
                        // the first approach, or the retract at the end: as they were
                        for q in &paths[k].moves[i..j] {
                            out.push(*q);
                        }
                        pos = Some(b);
                        i = j;
                    }
                }
                from = None;
                continue;
            }
            pos = Some(m.end(pos.unwrap_or(DVec3::ZERO)));
            out.push(m);
            from = None;
            i += 1;
        }
        sim.run_to(base + n, usize::MAX);
        base += n;
        // where its last cut ended (before its retract)
        let last_cut = out.iter().rev().find(|m| !matches!(m, Move::Rapid(_))).map(|_| {
            let mut p = DVec3::ZERO;
            let mut last = DVec3::ZERO;
            for m in &out {
                p = m.end(p);
                if !matches!(m, Move::Rapid(_)) {
                    last = p;
                }
            }
            last
        });
        paths[k].moves = out;
        prev = last_cut.map(|p| (k, p));
    }
    // The stock map the links were planned on has its own grid: a sliver of stock thinner than a cell (where two
    // passes only just met) can be there in one grid and not in another. Check the links the way the post's
    // simulation does, and lift any that still runs a rapid into the stock right up for that move.
    let safe = Ctx::new(cam, part).safe().max(sim.max_h as f64 + cam.clearance.max(1.0));
    for round in 0..3 {
        let mut check = crate::cam_sim::Sim::new(cam, part, stock, &paths, 500);
        if let Some(m) = stock_in {
            check.start_from(m);
        }
        check.run_to(usize::MAX, usize::MAX);
        let mut hits: Vec<usize> = check.events.iter().filter(|e| e.kind == crate::cam_sim::EventKind::Rapid).map(|e| e.step).collect();
        if hits.is_empty() {
            break;
        }
        hits.sort_unstable();
        hits.dedup();
        // global move index -> (path, move)
        let mut starts = vec![];
        let mut acc = 0usize;
        for tp in &paths {
            starts.push(acc);
            acc += tp.moves.len();
        }
        let mut runs: Vec<(usize, usize, usize)> = vec![];
        for h in hits {
            let k = match starts.binary_search(&h) {
                Ok(k) => k,
                Err(k) => k - 1,
            };
            let mv = &paths[k].moves;
            let i = h - starts[k];
            if !matches!(mv.get(i), Some(Move::Rapid(_))) {
                continue;
            }
            let (mut a, mut b) = (i, i + 1);
            while a > 0 && matches!(mv[a - 1], Move::Rapid(_)) {
                a -= 1;
            }
            while b < mv.len() && matches!(mv[b], Move::Rapid(_)) {
                b += 1;
            }
            if !runs.contains(&(k, a, b)) {
                runs.push((k, a, b));
            }
        }
        // from the back, so the indices of the runs still to do hold
        runs.sort_by(|x, y| y.cmp(x));
        for (k, a, b) in runs {
            let from = if a > 0 {
                paths[k].moves[..a].iter().fold(DVec3::ZERO, |p, m| m.end(p))
            } else {
                // the run starts the path: from where the one before ended
                (0..k).rev().find_map(|j| paths[j].moves.iter().rev().find(|m| !matches!(m, Move::Dwell(_))).map(|m| m.end(DVec3::ZERO))).unwrap_or(DVec3::new(0.0, 0.0, safe))
            };
            let to = paths[k].moves[b - 1].end(DVec3::ZERO);
            let mut run = vec![Move::Rapid([from.x, from.y, safe]), Move::Rapid([to.x, to.y, safe])];
            if round == 0 {
                run.push(Move::Rapid(to.to_array()));
            } else {
                // still into the stock coming down: feed the way down from the top of the stock
                let f = cam.tool(paths[k].tool).map(|t| t.plunge_feed).unwrap_or(300.0);
                run.push(Move::Rapid([to.x, to.y, sim.max_h as f64 + 1.0]));
                run.push(Move::Feed(to.to_array(), f));
            }
            paths[k].moves.splice(a..b, run);
            if a == 0 {
                paths[k].linked = false;
            }
        }
    }
    for tp in paths.iter_mut() {
        if let Some(tool) = cam.path_tool(tp.op, tp.tool) {
            stats(tp, &tool, cam);
        }
    }
    paths
}

fn stats(tp: &mut Toolpath, tool: &Tool, cam: &Cam) {
    let rapid_rate = cam.rapid_rate.max(1.0);
    let rapid_z = if cam.machine.rapid_z > 0.0 { cam.machine.rapid_z } else { rapid_rate };
    let mut p = DVec3::ZERO;
    let mut first = true;
    let (mut cut, mut rapid, mut time) = (0.0, 0.0, 0.0);
    for m in &tp.moves {
        let q = m.end(p);
        let d = if first { 0.0 } else { p.distance(q) };
        match *m {
            Move::Rapid(_) => {
                rapid += d;
                // the axes move together: the slower of them sets the time
                let (dxy, dz) = if first { (0.0, 0.0) } else { ((q - p).truncate().length(), (q.z - p.z).abs()) };
                time += (dxy / rapid_rate).max(dz / rapid_z) * 60.0;
            }
            Move::Feed(_, f) => {
                cut += d;
                time += d / f.max(1.0) * 60.0;
            }
            Move::Tap { z, r, pitch } => {
                // a tapping head unscrews the tap faster than it went in
                let back = if tool.tapping_head { 1.0 / tool.reverse_ratio.max(0.1) } else { 1.0 };
                let l = (r - z).abs();
                cut += 2.0 * l;
                time += l * (1.0 + back) / (cam.spindle_rpm(tool).max(1.0) * pitch.max(0.01)) * 60.0 + 1.0;
            }
            Move::Dwell(s) => time += s,
        }
        first = false;
        p = q;
    }
    tp.cut_length = cut;
    tp.rapid_length = rapid;
    tp.time = time;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::{Cam, OpKind, Side, ToolKind};

    #[test]
    fn a_ramp_back_over_the_entry_is_found() {
        // a lead-in's back-and-forth ramp: from (0, 0, 5) out to (2, 1) and back, 1 mm lower each way, then the arc
        let mut m = vec![];
        for k in 0..3 {
            let z = 5.0 - 2.0 * k as f64;
            m.push(Move::Feed([2.0, 1.0, z - 1.0], 2400.0));
            m.push(Move::Feed([0.0, 0.0, z - 2.0], 2400.0));
        }
        m.push(Move::Feed([3.0, 3.0, -1.0], 2400.0));
        m.push(Move::Feed([20.0, 3.0, -1.0], 2400.0));
        assert_eq!(ramp_back_to(&m, 0, DVec3::new(0.0, 0.0, 5.0), 8.0), Some((5, -1.0, 2400.0)));
        // a ramp that runs off along the wall instead never comes back: left as it is
        let along = vec![Move::Feed([5.0, 0.0, 4.0], 2400.0), Move::Feed([10.0, 0.0, 3.0], 2400.0)];
        assert_eq!(ramp_back_to(&along, 0, DVec3::new(0.0, 0.0, 5.0), 8.0), None);
    }

    #[test]
    fn cuts_finish_one_region_before_the_next() {
        // two areas 100 apart, cut level by level and alternating between them, as a path that bounces would
        let cut = |x: f64, z: f64| {
            vec![
                Move::Rapid([x, 0.0, 10.0]),
                Move::Rapid([x, 0.0, 1.0]),
                Move::Feed([x, 0.0, z], 300.0),
                Move::Feed([x + 10.0, 0.0, z], 1000.0),
                Move::Feed([x + 10.0, 5.0, z], 1000.0),
                // through what it cleared at depth: still the same cut
                Move::Rapid([x, 5.0, z]),
                Move::Feed([x + 10.0, 5.0, z], 1000.0),
                Move::Rapid([x + 10.0, 5.0, 10.0]),
            ]
        };
        let mut moves = vec![];
        for (x, z) in [(0.0, -1.0), (100.0, -1.0), (0.0, -2.0), (100.0, -2.0)] {
            moves.extend(cut(x, z));
        }
        moves.push(Move::Rapid([0.0, 0.0, 20.0]));
        let out = order_by_region(&moves, 3.0);
        // where each plunge lands: area A's two levels, then area B's, depth order kept
        let mut p = DVec3::ZERO;
        let mut plunges = vec![];
        let mut ends = vec![];
        for m in &out {
            let q = m.end(p);
            if let Move::Feed(_, f) = m {
                if *f == 300.0 {
                    plunges.push((q.x, q.z));
                }
                // every cut keeps its direction: +X
                if (q.y - p.y).abs() < 1e-9 && (q.z - p.z).abs() < 1e-9 {
                    assert!(q.x > p.x, "{p} -> {q}");
                }
            }
            // a rapid down into the work only over the cut's own entry or through what it cleared
            if let Move::Rapid(_) = m {
                assert!(q.z >= -2.0);
            }
            ends.push(q);
            p = q;
        }
        assert_eq!(plunges, vec![(0.0, -1.0), (0.0, -2.0), (100.0, -1.0), (100.0, -2.0)]);
        // the same moves, reordered, and the way across at the clearance height (20 here)
        assert_eq!(out.iter().filter(|m| !matches!(m, Move::Rapid(_))).count(), moves.iter().filter(|m| !matches!(m, Move::Rapid(_))).count());
        let across = ends.windows(2).find(|w| (w[0].x - w[1].x).abs() > 50.0).unwrap();
        assert!(across[0].z >= 20.0 && across[1].z >= 20.0, "{across:?}");
        assert_eq!(*ends.last().unwrap(), DVec3::new(0.0, 0.0, 20.0));
    }

    #[test]
    fn cycle_time_counts_spin_ups_and_moves_between_operations() {
        let mut c = Cam { rapid_rate: 6000.0, ..Default::default() };
        c.machine.spin_up = 5.0;
        c.machine.rapid_z = 0.0;
        let a = c.add_tool(crate::cam::Tool { rpm: 10000.0, ..Default::default() });
        let b = c.add_tool(crate::cam::Tool { rpm: 12000.0, ..Default::default() });
        let tp = |tool: u64, from: [f64; 3], to: [f64; 3]| Toolpath { tool, moves: vec![Move::Rapid(from), Move::Feed(to, 600.0)], time: 10.0, ..Default::default() };
        // two operations with the same tool, then another tool 100 mm away
        let paths = vec![tp(a, [0.0, 0.0, 5.0], [10.0, 0.0, 5.0]), tp(a, [10.0, 0.0, 5.0], [0.0, 0.0, 5.0]), tp(b, [100.0, 0.0, 5.0], [0.0, 0.0, 5.0])];
        // 3 × 10 s of moves, 2 spindle starts, 100 mm at 6000 mm/min = 1 s
        assert!((cycle_time(&c, &paths) - 41.0).abs() < 1e-9, "{}", cycle_time(&c, &paths));
        // empty toolpaths and ones without a tool aren't in the program
        let mut more = paths.clone();
        more.insert(1, Toolpath { tool: a, ..Default::default() });
        more.push(tp(999, [0.0; 3], [1.0, 0.0, 0.0]));
        assert!((cycle_time(&c, &more) - 41.0).abs() < 1e-9);
    }

    /// A 60 × 40 × 10 plate with a 20 × 20 pocket 5 deep, as upward triangles on a 1 mm grid plus a bottom.
    fn plate() -> Part {
        let mut tris = vec![];
        for j in 0..40 {
            for i in 0..60 {
                let (x, y) = (i as f64, j as f64);
                let z = if (10..30).contains(&i) && (10..30).contains(&j) { 5.0 } else { 10.0 };
                let p = |dx: f64, dy: f64| DVec3::new(x + dx, y + dy, z);
                tris.push([p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0)]);
                tris.push([p(0.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)]);
            }
        }
        tris.push([DVec3::ZERO, DVec3::new(0.0, 40.0, 0.0), DVec3::new(60.0, 0.0, 0.0)]);
        Part::new(&tris, &[], "top")
    }

    fn setup() -> (Cam, u64) {
        let mut c = Cam::default();
        let t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap();
        let id = c.add_tool(t);
        (c, id)
    }

    #[test]
    fn tools_sharing_a_number_are_flagged() {
        let part = plate();
        let (mut c, t) = setup();
        let u = c.add_tool(crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 3.0).unwrap());
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Contour, u);
        let ok = generate(&c, &part);
        assert!(ok.iter().all(|p| p.warnings.iter().all(|w| !w.contains("also the number"))), "{:?}", ok[0].warnings);
        let n = c.tool(t).unwrap().number;
        c.tools.iter_mut().find(|x| x.id == u).unwrap().number = n;
        let both = generate(&c, &part);
        assert!(both.iter().all(|p| p.warnings.iter().any(|w| w.contains(&format!("T{n} is also the number")))), "{:?}", both[1].warnings);
    }

    fn feeds(tp: &Toolpath) -> Vec<DVec3> {
        tp.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(DVec3::from(*p)) } else { None }).collect()
    }

    #[test]
    fn shadow_and_pockets() {
        let part = plate();
        let g = Geo::new(&part);
        let above_floor = g.shadow(5.0);
        assert!((shapes_area(&above_floor) - (2400.0 - 400.0)).abs() < 1.0);
        assert!((shapes_area(&g.shadow(7.0)) - 2000.0).abs() < 1.0);
        assert!(inside(&g.shadow(7.0), [5.0, 5.0]) && !inside(&g.shadow(7.0), [20.0, 20.0]));
        assert_eq!(g.floors(), vec![10.0, 5.0]);
    }

    #[test]
    fn contour_leads_on_and_off_with_arcs() {
        let part = plate();
        let (mut c, t) = setup();
        let id = c.add_op(OpKind::Contour, t);
        c.op_mut(id).unwrap().lead = 3.0;
        c.op_mut(id).unwrap().side = Side::Inside;
        // the pocket is still solid, so this slots it: that's allowed here (without, it warns)
        c.op_mut(id).unwrap().max_engagement = Some(1.0);
        let tp = generate(&c, &part).remove(0);
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        let pts = feeds(&tp);
        // every point keeps the tool off the pocket walls (centre at least 3 mm in from them)
        for p in &pts {
            let wall = (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(30.0 - p.y);
            assert!(wall > 3.0 - 0.02, "{p}");
        }
        // the first cut: ramp down back and forth along the lead-in (no straight plunge into the stock), then a
        // quarter circle of radius 3 at depth arriving 0.03 outside the wall offset (3 mm in): the lap before the
        // light last one
        let i = (1..pts.len() - 13)
            .find(|&i| pts[i].truncate().distance(pts[0].truncate()) < 1e-9 && pts[i + 1..i + 13].iter().all(|p| (p.z - pts[i].z).abs() < 1e-9))
            .unwrap();
        assert!(pts[i].z < pts[0].z - 1.0);
        assert!(pts[..=i].windows(2).all(|w| w[0].z - w[1].z <= 0.5 * w[0].truncate().distance(w[1].truncate()) + 1e-9), "plunges");
        let lead: Vec<DVec3> = pts[i..i + 13].to_vec();
        let end = lead[12];
        let wall = (end.x - 10.0).min(30.0 - end.x).min(end.y - 10.0).min(30.0 - end.y);
        assert!((wall - 3.03).abs() < 0.005, "{end}");
        let chord = lead[0].truncate().distance(end.truncate());
        assert!((chord - 3.0 * 2f64.sqrt()).abs() < 0.05, "{chord}");
        assert!(lead.iter().all(|p| (p.z - lead[0].z).abs() < 1e-9));
        // and it ends on the way out, off the wall again
        let last = *pts.last().unwrap();
        let wall = (last.x - 10.0).min(30.0 - last.x).min(last.y - 10.0).min(30.0 - last.y);
        assert!(wall > 4.0, "{last}");
        // then the last lap on the wall offset itself
        assert!(pts.iter().any(|p| {
            let wall = (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(30.0 - p.y);
            (wall - 3.0).abs() < 1e-6 && (p.z - lead[0].z).abs() < 1e-9
        }));
        // no room for a 20 mm lead: a smaller arc that fits, still not ramping along the wall
        c.op_mut(id).unwrap().lead = 20.0;
        let smaller = feeds(&generate(&c, &part).remove(0));
        let wall = |p: &DVec3| (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(30.0 - p.y);
        assert!(smaller.windows(2).all(|w| !(w[1].z < w[0].z - 1e-6 && wall(&w[1]) < 3.2)), "comes down on the wall");
        assert!(smaller.iter().all(|p| wall(p) > 3.0 - 0.02));
    }

    /// The plate's outline from outside, cut to size: the seam is in the middle of a long side, the tool comes
    /// down clear of the wall, runs on past the start, and rises as it leads off; a seam point moves the start;
    /// a spring pass goes round once more.
    #[test]
    fn contour_seam_overlaps_and_lifts_off() {
        let part = plate();
        let (mut c, t) = setup();
        let id = c.add_op(OpKind::Contour, t);
        c.op_mut(id).unwrap().max_engagement = Some(1.0);
        assert!((c.op_mut(id).unwrap().lead - 3.0).abs() < 1e-9, "new contours lead on with half the tool's diameter");
        let tp = generate(&c, &part).remove(0);
        let pts = feeds(&tp);
        let d = |p: &DVec3| (-p.x).max(p.x - 60.0).max(0.0).hypot((-p.y).max(p.y - 40.0).max(0.0));
        let low = pts.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        // never a Z move on the wall: every move down keeps a quarter of the lead (0.75 mm) off the wall offset
        for w in pts.windows(2) {
            if w[1].z < w[0].z - 1e-6 {
                assert!(d(&w[1]) > 3.0 + 0.25 * 3.0 - 0.05, "comes down at {} ({} off the wall)", w[1], d(&w[1]));
            }
        }
        // the light last lap on the wall offset, started in the middle of a long side (x 30), run on 1.5 past it
        let on_wall: Vec<&DVec3> = pts.iter().filter(|p| (p.z - low).abs() < 1e-9 && (d(p) - 3.0).abs() < 1e-3 && (p.y < 0.0 || p.y > 40.0)).collect();
        assert!(on_wall.iter().any(|p| (p.x - 31.5).abs() < 0.02 || (p.x - 28.5).abs() < 0.02), "no overlap past x 30");
        assert!(pts.iter().any(|p| (p.z - low).abs() < 1e-9 && (d(p) - 3.03).abs() < 1e-3), "no lap 0.03 out");
        // it leaves rising, away from the wall
        let last = *pts.last().unwrap();
        assert!(last.z > low + 0.5 && d(&last) > 3.0 + 2.0, "{last}");
        // a seam point at the left: it starts on the left side
        c.op_mut(id).unwrap().seam = Some([-100.0, 20.0]);
        let left = feeds(&generate(&c, &part).remove(0));
        let first_on_wall = left.iter().find(|p| (p.z - low).abs() < 1e-9 && (d(p) - 3.03).abs() < 1e-3).unwrap();
        assert!(first_on_wall.x < 0.0 && (first_on_wall.y - 20.0).abs() < 0.05, "{first_on_wall}");
        // a spring pass: once more round at the same size
        c.op_mut(id).unwrap().seam = None;
        let once = pts.iter().filter(|p| (p.z - low).abs() < 1e-9 && (d(p) - 3.0).abs() < 1e-3).count();
        c.op_mut(id).unwrap().spring_pass = true;
        let twice = feeds(&generate(&c, &part).remove(0)).iter().filter(|p| (p.z - low).abs() < 1e-9 && (d(p) - 3.0).abs() < 1e-3).count();
        assert!(twice >= 2 * once - 2, "{once} {twice}");
        // roughing (leaving stock) keeps a single lap
        c.op_mut(id).unwrap().spring_pass = false;
        c.op_mut(id).unwrap().leave = 0.3;
        let rough = feeds(&generate(&c, &part).remove(0));
        assert!(!rough.iter().any(|p| (d(p) - 3.33).abs() < 1e-3), "a lap outside a roughing pass");
    }

    /// A tool too big for any lead arc in the pocket: it ramps along the wall as before.
    #[test]
    fn contour_too_tight_for_a_lead_ramps() {
        let part = plate();
        let (mut c, _) = setup();
        let mut big = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap();
        big.diameter = 18.0;
        big.number = 18;
        let t = c.add_tool(big);
        let id = c.add_op(OpKind::Contour, t);
        c.op_mut(id).unwrap().side = Side::Inside;
        c.op_mut(id).unwrap().max_engagement = Some(1.0);
        // the 20 mm pocket leaves a 2 mm loop for the tool's centre: no room for even a 2.25 mm arc
        let pts = feeds(&generate(&c, &part).remove(0));
        assert!(!pts.is_empty());
        assert!(pts.windows(2).any(|w| w[1].z < w[0].z - 1e-6 && w[0].truncate().distance(w[1].truncate()) > 1e-6), "ramps along the wall");
        let wall = |p: &DVec3| (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(30.0 - p.y);
        assert!(pts.iter().all(|p| wall(p) > 9.0 - 0.02), "gouges");
    }

    /// An operation's own speed and feed replace its tool's for that operation only: in the moves, the
    /// post's S word (a change between two operations of one tool) and the stock simulation's cutter.
    #[test]
    fn an_operation_can_set_its_own_speed_and_feed() {
        let part = plate();
        let (mut c, t) = setup();
        c.post = "rcnc".into();
        let a = c.add_op(OpKind::Contour, t);
        let b = c.add_op(OpKind::Contour, t);
        let tool = c.tool(t).unwrap().clone();
        {
            let o = c.op_mut(b).unwrap();
            o.rpm = Some(tool.rpm * 0.75);
            o.feed = Some(777.0);
        }
        let paths = generate(&c, &part);
        let feeds_of = |tp: &Toolpath| tp.moves.iter().filter_map(|m| if let Move::Feed(_, f) = m { Some(*f) } else { None }).collect::<Vec<f64>>();
        let (fa, fb) = (feeds_of(&paths[0]), feeds_of(&paths[1]));
        assert!(fa.iter().any(|f| (f - c.feeds(&tool).0).abs() < 1e-6) && !fa.iter().any(|f| (f - 777.0).abs() < 1e-6), "{fa:?}");
        assert!(fb.iter().any(|f| (f - 777.0).abs() < 1e-6) && !fb.iter().any(|f| (f - c.feeds(&tool).0).abs() < 1e-6), "{fb:?}");
        assert_eq!(c.path_tool(paths[1].op, paths[1].tool).unwrap().rpm, tool.rpm * 0.75);
        assert_eq!(c.path_tool(paths[0].op, paths[0].tool).unwrap().rpm, tool.rpm);
        let _ = a;
        let g = crate::cam_post::gcode(&c, &paths, DVec3::ZERO, 30.0);
        let s_words: Vec<&str> = g.lines().filter(|l| l.starts_with('S')).collect();
        assert_eq!(s_words, vec![format!("S{} M3", tool.rpm.round()).as_str(), format!("S{} M3", (tool.rpm * 0.75).round()).as_str()], "{g}");
        assert_eq!(g.matches("M6").count(), 1, "one tool change for the two operations");
    }

    #[test]
    fn pocket_stays_inside() {
        let part = plate();
        let (mut c, t) = setup();
        c.add_op(OpKind::Pocket, t);
        let tp = &generate(&c, &part)[0];
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        let f = feeds(tp);
        assert!(!f.is_empty());
        let low = f.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        assert!((low - 5.0).abs() < 1e-6, "floor {low}");
        for p in f.iter().filter(|p| p.z < 10.0 - 1e-6) {
            assert!(p.x >= 13.0 - 1e-3 && p.x <= 27.0 + 1e-3 && p.y >= 13.0 - 1e-3 && p.y <= 27.0 + 1e-3, "{p}");
        }
    }

    #[test]
    fn contour_keeps_off_the_part() {
        let part = plate();
        let (mut c, t) = setup();
        let id = c.add_op(OpKind::Contour, t);
        c.op_mut(id).unwrap().side = Side::Outside;
        c.op_mut(id).unwrap().tabs = 4;
        let tp = &generate(&c, &part)[0];
        let f = feeds(tp);
        for p in &f {
            let dx = (-p.x).max(p.x - 60.0).max(0.0);
            let dy = (-p.y).max(p.y - 40.0).max(0.0);
            assert!(dx.hypot(dy) >= 3.0 - 0.01, "too close at {p}");
        }
        // cut-out: through the bottom, with tabs left standing
        let low = f.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        assert!(low < 0.0);
        assert!(f.iter().any(|p| (p.z - 2.0).abs() < 1e-6 && p.x > 0.0 && p.x < 60.0));
    }

    #[test]
    fn simulation_finds_no_gouges_and_flags_rapids() {
        let part = plate();
        let (mut c, t) = setup();
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Clear, t);
        c.add_op(OpKind::Pocket, t);
        c.add_op(OpKind::Contour, t);
        let paths = generate(&c, &part);
        let stock = stock_box(&c, &part);
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, &paths, 300);
        sim.run_to(usize::MAX, usize::MAX);
        // (the pocket's first pass is a full-width slot at the tool's default step-down: heavier than the load
        // check lets a light mill take, which is its own business)
        assert!(sim.events.iter().all(|e| e.kind == crate::cam_sim::EventKind::Overload), "{:?}", sim.events);
        assert!(sim.events.iter().all(|e| e.op == 3), "only the pocket's slot: {:?}", sim.events);
        assert!(sim.removed > 1000.0);
        // a rapid straight through the stock is a crash
        let bad = Toolpath { op: 1, tool: t, moves: vec![Move::Rapid([-10.0, 20.0, 5.0]), Move::Rapid([70.0, 20.0, 5.0])], ..Default::default() };
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, &[bad], 300);
        sim.run_to(usize::MAX, usize::MAX);
        assert!(sim.events.iter().any(|e| e.kind == crate::cam_sim::EventKind::Rapid));
        assert!(sim.events.iter().any(|e| e.kind == crate::cam_sim::EventKind::Gouge));
    }

    #[test]
    fn face_runs_along_y_with_an_angle_of_90() {
        let part = plate();
        let (mut c, t) = setup();
        let id = c.add_op(OpKind::Face, t);
        let along = |c: &Cam| {
            let tp = generate(c, &part).remove(0);
            let (mut dx, mut dy) = (0.0, 0.0);
            let mut prev = DVec3::ZERO;
            for m in &tp.moves {
                let p = m.end(prev);
                if let Move::Feed(..) = m {
                    dx += (p.x - prev.x).abs();
                    dy += (p.y - prev.y).abs();
                }
                prev = p;
            }
            (dx, dy)
        };
        let (dx, dy) = along(&c);
        assert!(dx > 3.0 * dy, "along X by default: {dx} {dy}");
        c.op_mut(id).unwrap().angle = 90.0;
        let (dx, dy) = along(&c);
        assert!(dy > 3.0 * dx, "along Y at 90: {dx} {dy}");
    }

    #[test]
    fn toolpaths_lift_over_fixtures() {
        let part = plate();
        let (mut c, t) = setup();
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Clear, t);
        let (stock, o) = (stock_box(&c, &part), work_origin(&c, &part));
        c.fixtures.push(crate::cam::place_fixture("Toe clamp", [20.0, 16.0, 12.0], stock, o, 0));
        let (_, lo, hi) = fixture_boxes(&c, &part, o).remove(0);
        // over the left edge of the stock, on top of it
        assert!(lo.x < stock.0.x && hi.x > stock.0.x && (lo.z - stock.1.z).abs() < 1e-9);
        let paths = generate(&c, &part);
        let zc = hi.z + c.retract.max(1.0);
        for tp in &paths {
            assert!(tp.warnings.iter().any(|w| w.contains("fixture")), "{:?}", tp.warnings);
            let mut prev = DVec3::new(0.0, 0.0, zc + 10.0);
            for m in &tp.moves {
                let p = m.end(prev);
                for k in 0..=10 {
                    let q = prev.lerp(p, k as f64 / 10.0);
                    let near = q.x > lo.x - 4.0 + 1e-6 && q.x < hi.x + 4.0 - 1e-6 && q.y > lo.y - 4.0 + 1e-6 && q.y < hi.y + 4.0 - 1e-6;
                    assert!(!near || q.z > zc - 1e-6, "{} passes through the clamp at {q}", tp.op);
                }
                prev = p;
            }
        }
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, &paths, 300);
        sim.run_to(usize::MAX, usize::MAX);
        assert!(!sim.events.iter().any(|e| e.kind == crate::cam_sim::EventKind::Fixture), "{:?}", sim.events.first());
        // a move straight through it is a hit
        let y = (lo.y + hi.y) / 2.0;
        let bad = Toolpath { op: 1, tool: t, moves: vec![Move::Rapid([lo.x - 20.0, y, hi.z - 2.0]), Move::Rapid([hi.x + 20.0, y, hi.z - 2.0])], ..Default::default() };
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, &[bad], 300);
        sim.run_to(usize::MAX, usize::MAX);
        assert!(sim.events.iter().any(|e| e.kind == crate::cam_sim::EventKind::Fixture));
        // a fixture clearance of its own: the tool comes down to just over the clamp, no lower
        c.fixture_clearance = 0.3;
        let zc = hi.z + 0.3;
        let lowest = generate(&c, &part)
            .iter()
            .flat_map(|tp| {
                let mut prev = DVec3::new(0.0, 0.0, zc + 10.0);
                tp.moves.iter().map(move |m| {
                    prev = m.end(prev);
                    prev
                }).collect::<Vec<_>>()
            })
            .filter(|q| q.x > lo.x - 4.0 + 1e-6 && q.x < hi.x + 4.0 - 1e-6 && q.y > lo.y - 4.0 + 1e-6 && q.y < hi.y + 4.0 - 1e-6)
            .map(|q| q.z)
            .fold(f64::INFINITY, f64::min);
        assert!((lowest - zc).abs() < 1e-6, "lowest over the clamp {lowest}, want {zc}");
    }

    #[test]
    fn gcode_dialects() {
        let part = plate();
        let (mut c, t) = setup();
        c.add_op(OpKind::Contour, t);
        let paths = generate(&c, &part);
        let (origin, top) = {
            let ctx = Ctx::new(&c, &part);
            (ctx.origin, ctx.stock.1.z)
        };
        for post in ["grbl", "linuxcnc", "fanuc", "mach3", "rcnc"] {
            c.post = post.into();
            let g = crate::cam_post::gcode(&c, &paths, origin, top + c.clearance);
            assert!(g.contains("G21") && g.contains("M30"), "{post}");
            assert_eq!(g.contains("T1 M6"), post != "grbl", "{post}");
            // a second station's setup runs in its own work offset
            let mut w = c.clone();
            w.work_offset = 2;
            let g2 = crate::cam_post::gcode(&w, &paths, origin, top + c.clearance);
            assert!(g2.lines().any(|l| l == "G55") && !g2.lines().any(|l| l == "G54"), "{post}: {g2}");
            if post == "rcnc" {
                // rcnc's own M6 handles the pause and the measuring
                assert!(!g.contains("MSG") && !g.contains("G43"), "{g}");
                // rcnc's M6 measures and its M3 waits for the spindle: no probe cycle or dwell on top
                let mut p = c.clone();
                p.probe.mode = "setter".into();
                p.machine.spin_up = 5.0;
                let g = crate::cam_post::gcode(&p, &paths, origin, top + c.clearance);
                assert!(!g.contains("G38") && !g.contains("G31") && !g.contains("G4 "), "{g}");
                // probing the stock's top and middle becomes rcnc's Find stock, after loading the probe and before
                // the first cutter, with no spindle in between
                let mut t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Probe).unwrap();
                t.number = 99;
                let probe = p.add_tool(t);
                for cycle in [crate::cam::ProbeCycle::Z, crate::cam::ProbeCycle::Boss] {
                    let id = p.add_op(OpKind::Probe, probe);
                    let mut o = p.ops.pop().unwrap();
                    assert_eq!(o.id, id);
                    o.probing = Some(crate::cam::ProbeOp { cycle, ..Default::default() });
                    let at = p.ops.iter().filter(|o| o.kind == OpKind::Probe).count();
                    p.ops.insert(at, o);
                }
                let paths = generate(&p, &part);
                let stock = Ctx::new(&p, &part).stock;
                let g = crate::cam_post::gcode_noted(&p, &paths, origin, top + c.clearance, None, Some(stock));
                let size = stock.1 - stock.0;
                let at = DVec3::new((stock.0.x + stock.1.x) / 2.0, (stock.0.y + stock.1.y) / 2.0, stock.1.z) - origin;
                let n = crate::cam_post::num;
                let want = format!("(RCNC FIND_STOCK SIZE={},{},{} AT={},{},{} TOL=1 DEPTH=4)", n(size.x), n(size.y), n(size.z), n(at.x), n(at.y), n(at.z));
                let (probe, req, cutter) = (g.find("T99 M6").unwrap(), g.find(&want).expect(&g), g.find("T1 M6").unwrap());
                assert!(probe < req && req < cutter, "{g}");
                assert_eq!(g.matches("FIND_STOCK").count(), 1, "one request for both: {g}");
                assert!(!g.contains("G31"), "{g}");
                assert!(!g[..cutter].contains("M3"), "no spindle with the probe in: {g}");
                // X from the fixed jaw, set on either probe
                p.ops.iter_mut().filter(|o| o.kind == OpKind::Probe).last().unwrap().probing.as_mut().unwrap().jaw_x = -1.0;
                p.ops.iter_mut().filter(|o| o.kind == OpKind::Probe).last().unwrap().probing.as_mut().unwrap().jaw_z = 2.5;
                let g = crate::cam_post::gcode_noted(&p, &paths, origin, top + c.clearance, None, Some(stock));
                assert!(g.contains(&want.replace(")", " JAWX=-1 JAWZ=2.5)")), "{g}");
                let pr = p.ops.iter_mut().filter(|o| o.kind == OpKind::Probe).last().unwrap().probing.as_mut().unwrap();
                pr.jaw_at = Some(-9.8);
                pr.find_size = Some([25.4, 98.0]);
                let g = crate::cam_post::gcode_noted(&p, &paths, origin, top + c.clearance, None, Some(stock));
                let want2 = format!("(RCNC FIND_STOCK SIZE=25.4,98,{} AT={},{},{} TOL=1 DEPTH=4 JAWX=-1 JAWZ=2.5 JAWAT=-9.8)", n(size.z), n(at.x), n(at.y), n(at.z));
                assert!(g.contains(&want2), "{g}");
            }
            // work zero at the stock top: nothing cuts above Z0
            for l in g.lines().filter(|l| l.starts_with("G1") || l.starts_with('Z')) {
                if let Some(z) = l.split_whitespace().find(|w| w.starts_with('Z')) {
                    let z: f64 = z[1..].parse().unwrap();
                    assert!(z <= c.clearance + 1e-6, "{l}");
                }
            }
        }
    }

    #[test]
    fn gcode_probing_and_custom_text() {
        let part = plate();
        let (mut c, t) = setup();
        let small = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 3.0).unwrap();
        let t2 = c.add_tool(small);
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Pocket, t2);
        let paths = generate(&c, &part);
        let ctx = Ctx::new(&c, &part);
        let (origin, safe) = (ctx.origin, ctx.safe());
        // touch plate on GRBL: each tool probes down twice and sets the work Z from the plate's top
        c.probe.mode = "plate".into();
        c.probe.plate_at = [5.0, 5.0, 0.0];
        c.probe.plate_thickness = 10.0;
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert_eq!(g.matches("G38.2").count(), 4, "{g}");
        assert_eq!(g.matches("G10 L20 P1 Z10").count(), 2);
        assert!(g.find("G53 G0 Z0").unwrap() < g.find("G38.2").unwrap());
        // tool setter on LinuxCNC: the first tool is the reference, every tool gets a length offset
        c.post = "linuxcnc".into();
        c.probe.mode = "setter".into();
        c.probe.setter = [-5.0, -10.0, -20.0];
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert_eq!(g.matches("#<_tool_ref> = #5063").count(), 1, "{g}");
        assert_eq!(g.matches("G43.1 Z[#5063 - #<_tool_ref>]").count(), 2);
        assert!(g.contains("G53 G0 X-5 Y-10") && !g.contains("G43 H"));
        let reference = g.find("#<_tool_ref>").unwrap();
        assert!(g[..reference].contains("T1 M6") && !g[..reference].contains("T2 M6"));
        // Fanuc probes with G31 and keeps the offsets in the tool table
        c.post = "fanuc".into();
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(g.contains("G31") && !g.contains("G38.2") && g.contains("G10 L10 P2 R[#5063 - #500]") && g.contains("G43 H2"));
        // the user's own G-code, with placeholders
        c.probe.mode = "off".into();
        c.post = "grbl".into();
        c.post_text.header = "G0 G53 Z0\n(program {program} {setup})".into();
        c.post_text.tool_change = "M0 (load T{tool}: {desc} at {rpm} rpm)".into();
        c.post_text.footer = "G53 G0 X0 Y0".into();
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(g.contains(&format!("(program {} {})", c.program, c.name)), "{g}");
        assert!(g.contains("M0 (load T2: D3 flat end mill"), "{g}");
        assert_eq!(g.matches("(load T").count(), 2);
        let end = g.find("G53 G0 X0 Y0").unwrap();
        assert!(end < g.find("M30").unwrap() && end > g.find("M5").unwrap());
    }
    #[test]
    fn mach3_program_for_a_manual_high_speed_mill() {
        let part = plate();
        let (mut c, t) = setup();
        let small = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 3.0).unwrap();
        let t2 = c.add_tool(small);
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Pocket, t2);
        c.post = "mach3".into();
        c.max_rpm = 10000.0;
        c.min_rpm = 6000.0;
        let rpm = c.tool(t).unwrap().rpm;
        assert!(rpm > 10000.0, "{rpm}");
        let paths = generate(&c, &part);
        // above the top speed: run at it, with the feeds scaled down to the same chip load
        let feed = c.tool(t).unwrap().feed;
        let fastest = paths[0].moves.iter().filter_map(|m| if let Move::Feed(_, f) = m { Some(*f) } else { None }).fold(0.0, f64::max);
        assert!((fastest - feed * 10000.0 / rpm).abs() < 1.0, "{fastest} vs {feed}");
        assert!(paths[0].warnings.iter().any(|w| w.contains("tops out")), "{:?}", paths[0].warnings);
        let (origin, safe) = {
            let ctx = Ctx::new(&c, &part);
            (ctx.origin, ctx.safe())
        };
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(g.contains("G21 G17 G40 G49 G80 G90 G91.1 G94"), "{g}");
        assert!(g.contains("S10000 M3") && !g.contains(&format!("S{} M3", rpm.round())));
        // the spin-up wait is the machine's, in seconds (and said so for Mach3's profile setting)
        assert!(g.contains("G4 P5\n") && g.contains("G4 dwell param in milliseconds"));
        // retracts in machine coordinates before each tool change and at the end
        let t1 = g.find("T1 M6").unwrap();
        let t2 = g.find("T2 M6").unwrap();
        assert!(g[..t1].contains("G53 G0 Z0") && g[t1..t2].contains("G53 G0 Z0"));
        let end = g.rfind("G53 G0 Z0").unwrap();
        assert!(end > t2 && end < g.find("M30").unwrap());
        // no tool changer: the operator is told what to load, and the program waits
        assert!(g.contains("(MSG,change to T2: D3 flat end mill") && g[..t2].ends_with("M0\n"), "{g}");
        // aluminium: flood coolant by default
        assert!(g.contains("M8") && g.contains("M9"));
        c.coolant = crate::cam::Coolant::Off;
        let dry = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(!dry.contains("M8"));
        // LinuxCNC retracts the same way; Fanuc returns home with G28; GRBL stays in work coordinates
        c.post = "linuxcnc".into();
        assert!(crate::cam_post::gcode(&c, &paths, origin, safe).contains("G53 G0 Z0"));
        c.post = "fanuc".into();
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(g.contains("G91 G28 Z0") && g.contains("G4 P5000") && !g.contains("G53 G0 Z0"));
        c.post = "grbl".into();
        assert!(!crate::cam_post::gcode(&c, &paths, origin, safe).contains("G53"));
    }

    #[test]
    fn tapping_on_a_high_speed_spindle() {
        let part = plate();
        let (mut c, _) = setup();
        c.post = "mach3".into();
        c.min_rpm = 12000.0;
        let mut tap = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Tap && t.diameter == 5.0).unwrap();
        crate::cam::suggest(&mut tap, "Aluminium", c.max_rpm, c.min_rpm);
        let t = c.add_tool(tap);
        let id = c.add_op(OpKind::Tap, t);
        c.op_mut(id).unwrap().points = vec![[30.0, 20.0]];
        c.op_mut(id).unwrap().bottom = Some(-8.0);
        let paths = generate(&c, &part);
        // a G84 that reverses a spindle with no torque at that speed: an error, not just a warning
        assert!(paths[0].warnings.iter().any(|w| w.contains("below the spindle's 12000 rpm minimum") && w.contains("tapping head")), "{:?}", paths[0].warnings);
        assert!(tap_danger(&c, c.tool(t).unwrap()).is_some());
        let (origin, safe) = {
            let ctx = Ctx::new(&c, &part);
            (ctx.origin, ctx.safe())
        };
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(g.contains("G84") && g.contains("no rigid tapping"));
        // rcnc can't reverse the DMC2's spindle: stop over the hole for a hand tap instead of a G84
        c.post = "rcnc".into();
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(!g.contains("G84") && !g.contains("M4") && g.contains("tap by hand") && g.contains("M0"), "{g}");
        c.post = "mach3".into();
        // in a self-reversing tapping head: straight in and out with the spindle running forward (blind: stops two pitches short)
        c.tools.iter_mut().find(|x| x.id == t).unwrap().tapping_head = true;
        let paths = generate(&c, &part);
        assert!(tap_danger(&c, c.tool(t).unwrap()).is_none());
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(!g.contains("G84") && !g.contains("M4") && !g.contains("no rigid tapping"), "{g}");
        let tool = c.tool(t).unwrap();
        let f = tool.rpm * tool.pitch;
        assert!(g.contains(&format!("G1 Z-6.4 F{}", f.round())) && g.contains(&format!("F{}", (f * 1.75).round())), "{g}");
    }

    #[test]
    fn tight_loops_spiral_in_instead_of_plunging() {
        let tool = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap();
        let mut pb = Pb { tool: &tool, moves: vec![], pos: None, safe: 10.0, retract: 2.0, ramp: 3f64.to_radians(), feed: 1000.0, plunge: 300.0 };
        // a loop 0.8 across: far too short to ramp along
        let l: Contour = (0..16).map(|k| std::f64::consts::TAU * k as f64 / 16.0).map(|a| [20.0 + 0.4 * a.cos(), 20.0 + 0.4 * a.sin()]).collect();
        pb.cut_loop(&l, -3.0, 0.0, 0.0, None, None);
        let mut prev = DVec3::new(0.0, 0.0, 10.0);
        let mut down = 0;
        for m in &pb.moves {
            let p = m.end(prev);
            if matches!(m, Move::Feed(..)) && p.z < prev.z - 1e-9 && p.z < -1e-9 {
                down += 1;
                assert!(p.truncate().distance(prev.truncate()) > 1e-3, "plunges straight down at {p}");
                assert!(p.truncate().distance(DVec2::splat(20.0)) < 0.4 + 1e-6);
            }
            prev = p;
        }
        assert!(down > 10, "{down}");
        assert!((prev.z + 3.0).abs() < 1e-9);
    }

    #[test]
    fn tilted_setup_zero_is_on_the_block() {
        let part_in = PartInput { tris: plate().tris, ..Default::default() };
        let mut c = Cam { origin: crate::cam::Origin::StockTopCorner, ..Default::default() };
        c.stock.carried = Some([[-3.0, -3.0, 0.0], [63.0, 43.0, 11.0]]);
        c.tilt = [20.0, 0.0];
        let part = Part::from_input(&part_in, &c);
        let o = work_origin(&c, &part);
        let corners = tilted_block(&c, &part).unwrap();
        // the highest real corner of the block, not a corner of its (bigger) bounding box
        assert!(corners.iter().any(|k| k.distance(o) < 1e-9));
        assert!((o.z - stock_box(&c, &part).1.z).abs() < 1e-9);
        let (lo, hi) = stock_box(&c, &part);
        assert!(o.y > lo.y + 1.0 || o.y < hi.y - 1.0);
        c.origin = crate::cam::Origin::StockTopCenter;
        let o = work_origin(&c, &part);
        // the middle of the tilted top face: on the block's surface
        let n = part.rot * DVec3::Z;
        let top = part.rot * DVec3::new(30.0, 20.0, 11.0);
        assert!(o.distance(top) < 1e-9 && n.z > 0.9, "{o} {top}");
        assert!(work_zero_note(&c, &part).contains("highest corner is at"));
        assert!(c.setup_notes().iter().any(|n| n.contains("20° about X (back edge raised)")));
        // square setups keep the bounding box
        c.tilt = [0.0, 0.0];
        let part = Part::from_input(&part_in, &c);
        assert!(tilted_block(&c, &part).is_none());
    }

    #[test]
    fn hints_on_order_and_holding() {
        let part = plate();
        let (mut c, t) = setup();
        let ch = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Chamfer && t.diameter == 6.0).unwrap();
        let ch = c.add_tool(ch);
        c.add_op(OpKind::Chamfer, ch);
        let k = c.add_op(OpKind::Contour, t);
        c.op_mut(k).unwrap().tabs = 4;
        let h = hints(&c, &part, &generate(&c, &part));
        assert!(h.iter().any(|h| h.starts_with("Chamfer 1 runs before Contour 1")), "{h:?}");
        assert!(h.iter().any(|h| h.contains("tabs hold")), "{h:?}");
        assert!(h.iter().any(|h| h.contains("below the stock bottom")), "{h:?}");
        c.ops.reverse();
        c.stock.side = 12.0;
        c.fixtures.push(Default::default());
        let h = hints(&c, &part, &generate(&c, &part));
        assert!(h.is_empty(), "{h:?}");
    }

    /// The rapid that brings the tool down before its first cut.
    fn entry(tp: &Toolpath) -> DVec3 {
        let k = tp.moves.iter().position(|m| matches!(m, Move::Feed(..))).unwrap();
        match tp.moves[k - 1] {
            Move::Rapid(p) => DVec3::from(p),
            ref m => panic!("{m:?}"),
        }
    }

    #[test]
    fn levels_above_the_part_dont_count_as_cut() {
        // 5 mm of stock over the part, 3 mm steps: the level at -3 has nothing to follow
        let part = plate();
        let (mut c, t) = setup();
        c.stock.top = 5.0;
        let id = c.add_op(OpKind::Contour, t);
        c.op_mut(id).unwrap().stepdown = Some(3.0);
        let tp = generate(&c, &part).remove(0);
        let top = stock_box(&c, &part).1.z;
        assert!(entry(&tp).z >= top - 1e-6, "{:?}", entry(&tp));
        let p = c.add_op(OpKind::Pocket, t);
        c.op_mut(p).unwrap().stepdown = Some(3.0);
        c.op_mut(id).unwrap().suppressed = true;
        let tp = generate(&c, &part).remove(0);
        assert!(entry(&tp).z >= top - 1e-6, "{:?}", entry(&tp));
    }

    #[test]
    fn helix_stays_inside_thin_loops() {
        let t = Tool { diameter: 6.0, ..Default::default() };
        let mut pb = Pb { tool: &t, moves: vec![], pos: None, safe: 20.0, retract: 2.0, ramp: 3f64.to_radians(), feed: 1000.0, plunge: 300.0 };
        // a 1.2 × 0.1 tool-centre region: its corners are 0.6 from the middle, its sides 0.05
        let l = vec![[0.0, 0.0], [1.2, 0.0], [1.2, 0.1], [0.0, 0.1]];
        if pb.helix_in(&l, 0.0, -3.0) {
            for m in &pb.moves {
                if let Move::Feed(p, _) = m {
                    assert!(p[1] >= -1e-6 && p[1] <= 0.1 + 1e-6, "{p:?}");
                }
            }
        }
    }

    /// A 60 × 40 × 10 plate with a thumb dish: a cone 8 in radius, its face 25° from flat (shallower than a
    /// 90° chamfer mill's 45°), as on the stick-out gauge.
    fn dished() -> Part {
        let mut tris = vec![];
        let z = |x: f64, y: f64| {
            let rho = (x - 30.0).hypot(y - 20.0);
            if rho < 8.0 {
                10.0 - (8.0 - rho) * 25f64.to_radians().tan()
            } else {
                10.0
            }
        };
        let n = 0.25;
        for j in 0..(40.0 / n) as i32 {
            for i in 0..(60.0 / n) as i32 {
                let (x, y) = (i as f64 * n, j as f64 * n);
                let p = |dx: f64, dy: f64| DVec3::new(x + dx, y + dy, z(x + dx, y + dy));
                tris.push([p(0.0, 0.0), p(n, 0.0), p(n, n)]);
                tris.push([p(0.0, 0.0), p(n, n), p(0.0, n)]);
            }
        }
        tris.push([DVec3::ZERO, DVec3::new(0.0, 40.0, 0.0), DVec3::new(60.0, 0.0, 0.0)]);
        Part::new(&tris, &[], "top")
    }

    #[test]
    fn a_chamfer_mill_leaves_out_an_edge_beside_a_shallow_face() {
        // the stick-out gauge's op 1 (2026-11-18): a 0.3 edge break round the dish's rim with the tip 1 mm
        // below the chamfer, as for a wall, cut the dish all round: the cone (45°) is steeper than the dish
        let part = dished();
        let mut c = Cam::default();
        let t = c.add_tool(crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Chamfer && t.diameter == 6.0).unwrap());
        let id = c.add_op(OpKind::Chamfer, t);
        let op = c.op_mut(id).unwrap();
        op.chamfer = 0.3;
        op.tip_offset = 1.0;
        op.side = Side::Both;
        let tp = generate(&c, &part).remove(0);
        assert_eq!(tp.warnings.iter().filter(|w| w.contains("left out the edge")).count(), 1, "{:?}", tp.warnings);
        // the outline is still broken, and nothing comes near the dish
        let mut p = DVec3::ZERO;
        let mut feeds = 0;
        for m in &tp.moves {
            p = m.end(p);
            if matches!(m, Move::Feed(..)) {
                feeds += 1;
                assert!(p.truncate().distance(DVec2::new(30.0, 20.0)) > 12.0, "{p}");
            }
        }
        assert!(feeds > 4);
        let stock = stock_box(&c, &part);
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, std::slice::from_ref(&tp), 400);
        sim.run_to(usize::MAX, usize::MAX);
        assert!(sim.events.is_empty(), "{:?}", sim.events);
        // what it used to do: the same pass round the rim, the tip 1.3 down and 1 in from it. The simulation
        // now calls that a gouge: the chamfer's allowance holds only beside a face falling away as steeply
        // as the cone, not over the whole dish
        let (r, z) = (8.0 - 1.0, 10.0 - 1.3);
        let mut moves = vec![Move::Rapid([30.0 + r, 20.0, 15.0]), Move::Feed([30.0 + r, 20.0, z], 200.0)];
        for k in 1..=72 {
            let a = k as f64 * 5f64.to_radians();
            moves.push(Move::Feed([30.0 + r * a.cos(), 20.0 + r * a.sin(), z], 1000.0));
        }
        moves.push(Move::Rapid([30.0 + r, 20.0, 15.0]));
        let old = Toolpath { op: id, tool: t, moves, ..Default::default() };
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, &[old], 400);
        sim.run_to(usize::MAX, usize::MAX);
        let deepest = sim.events.iter().filter(|e| e.kind == crate::cam_sim::EventKind::Gouge).map(|e| e.depth).fold(0.0, f64::max);
        assert!(deepest > 0.3, "{:?}", sim.events);
    }

    #[test]
    fn deep_chamfers_come_down_above_the_edge() {
        let part = plate();
        let mut c = Cam::default();
        let t = c.add_tool(crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Chamfer && t.diameter == 6.0).unwrap());
        let id = c.add_op(OpKind::Chamfer, t);
        c.retract = 1.0;
        c.op_mut(id).unwrap().chamfer = 2.0;
        let tp = generate(&c, &part).remove(0);
        assert!(entry(&tp).z >= part.hi.z + 1.0 - 1e-6, "{:?} {:?}", entry(&tp), tp.warnings);
    }

    #[test]
    fn drilling_feeds_at_the_plunge_rate() {
        let part = plate();
        let (mut c, t) = setup();
        let (feed, plunge) = (c.tool(t).unwrap().feed, c.tool(t).unwrap().plunge_feed);
        assert!(feed > plunge);
        let id = c.add_op(OpKind::Drill, t);
        let o = work_origin(&c, &part);
        c.op_mut(id).unwrap().points = vec![[30.0 - o.x, 20.0 - o.y]];
        c.op_mut(id).unwrap().bottom = Some(-3.0);
        c.op_mut(id).unwrap().peck = 1.0;
        let tp = generate(&c, &part).remove(0);
        let fs: Vec<f64> = tp.moves.iter().filter_map(|m| if let Move::Feed(_, f) = m { Some(*f) } else { None }).collect();
        assert!(!fs.is_empty() && fs.iter().all(|f| (*f - plunge).abs() < 1e-6), "{fs:?} {:?}", tp.warnings);
    }

    #[test]
    fn hot_cutting_speeds_are_flagged() {
        let part = plate();
        let (mut c, t) = setup();
        c.add_op(OpKind::Face, t);
        let ok = generate(&c, &part);
        assert!(ok[0].warnings.iter().all(|w| !w.contains("m/min")), "{:?}", ok[0].warnings);
        c.stock.material = "Stainless steel".into();
        let hot = generate(&c, &part);
        assert!(hot[0].warnings.iter().any(|w| w.contains("m/min")), "{:?}", hot[0].warnings);
    }

    /// The phone stand's plinth: a 74 × 74 outline with R10 corners, 30 high, standing on the origin.
    fn plinth() -> Part {
        let (h, half, rc) = (30.0, 37.0, 10.0);
        let mut outline: Vec<DVec2> = vec![];
        for (k, (sx, sy)) in [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].into_iter().enumerate() {
            let c = DVec2::new(sx * (half - rc), sy * (half - rc));
            for s in 0..=16 {
                let a = std::f64::consts::FRAC_PI_2 * (k as f64 + s as f64 / 16.0);
                outline.push(c + DVec2::new(a.cos(), a.sin()) * rc);
            }
        }
        let n = outline.len();
        let mut tris: Vec<[DVec3; 3]> = (0..n).map(|i| [DVec3::new(0.0, 0.0, h), outline[i].extend(h), outline[(i + 1) % n].extend(h)]).collect();
        tris.push([DVec3::new(-half, -half, 0.0), DVec3::new(-half, half, 0.0), DVec3::new(half, -half, 0.0)]);
        Part::new(&tris, &[], "top")
    }

    /// The cutting moves of a toolpath below the top (horizontal feeds), as (from, to).
    fn level_feeds(tp: &Toolpath, below: f64) -> Vec<(DVec3, DVec3)> {
        let mut p = DVec3::ZERO;
        let mut out = vec![];
        for m in &tp.moves {
            let q = m.end(p);
            if matches!(m, Move::Feed(..)) && q.z < below && (q.z - p.z).abs() < 1e-9 && q.truncate().distance(p.truncate()) > 0.05 {
                out.push((p, q));
            }
            p = q;
        }
        out
    }

    /// Length of the cutting moves (horizontal feeds below `below`) that climb and that cut conventional, from
    /// the simulated stock before each move: with a clockwise spindle a climb cut has the material it takes on
    /// the right of travel, a conventional one on the left; moves with material on both sides (slotting) or
    /// neither (air) don't count.
    fn climb_and_conventional(c: &Cam, part: &Part, tp: &Toolpath, below: f64) -> (f64, f64) {
        let r = c.tool(tp.tool).unwrap().radius();
        let stock = stock_box(c, part);
        let mut sim = crate::cam_sim::Sim::new(c, part, stock, std::slice::from_ref(tp), 400);
        let at = |sim: &crate::cam_sim::Sim, q: DVec2| -> f32 {
            let (i, j) = (((q.x - sim.x0) / sim.cell).round(), ((q.y - sim.y0) / sim.cell).round());
            if i < 0.0 || j < 0.0 || i as usize >= sim.nx || j as usize >= sim.ny {
                return crate::cam_stock::EMPTY;
            }
            sim.stock[j as usize * sim.nx + i as usize]
        };
        let (mut climb, mut conv) = (0.0, 0.0);
        let mut p = DVec3::ZERO;
        for (i, m) in tp.moves.iter().enumerate() {
            let q = m.end(p);
            if matches!(m, Move::Feed(..)) && q.z < below && (q.z - p.z).abs() < 1e-9 && q.truncate().distance(p.truncate()) > 0.05 {
                sim.run_to(i, usize::MAX);
                let d = (q - p).truncate().normalize();
                let mid = (p.truncate() + q.truncate()) / 2.0;
                // just inside the tool's reach on either side, a little ahead of the middle
                let side = |s: f64| (0..3).any(|k| at(&sim, mid + d * (0.25 * k as f64 * r) + d.perp() * s * (r - 0.3)) as f64 > q.z + 0.05);
                let (left, right) = (side(1.0), side(-1.0));
                let len = p.distance(q);
                match (left, right) {
                    (false, true) => climb += len,
                    (true, false) => conv += len,
                    _ => {}
                }
            }
            p = q;
        }
        (climb, conv)
    }

    #[test]
    fn roughing_round_the_part_climbs_all_the_way() {
        // Jonas (2026-11-18): adaptive and contours climb only, passes all the way round the part, corners
        // handled (the engagement at the stock's corners is checked in
        // contour_roughing_keeps_corner_engagement_under_the_limit)
        let part = plinth();
        let mut c = Cam::default();
        c.stock.size = Some([86.0, 86.0, 30.0]);
        c.stock.bottom = 0.0;
        let t = c.add_tool(crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap());
        let id = c.add_op(OpKind::Contour, t);
        {
            let o = c.op_mut(id).unwrap();
            o.side = Side::Outside;
            o.bottom = Some(-6.0);
            o.stepdown = Some(6.0);
        }
        assert!(c.op(id).unwrap().climb, "climb is the default");
        let top = stock_box(&c, &part).1.z;
        let tp = generate(&c, &part).remove(0);
        let (climb, conv) = climb_and_conventional(&c, &part, &tp, top - 1e-6);
        assert!(climb > 500.0 && conv < 0.02 * climb, "contour: {climb:.0} mm climb, {conv:.0} mm conventional");
        // passes all the way round: the outermost takes the square stock's corners, beyond the wall's
        let fs = level_feeds(&tp, top - 1e-6);
        let outer = fs.iter().map(|(_, q)| plinth_distance(q.truncate())).fold(0.0, f64::max);
        assert!(outer > 5.0, "{outer}");
        // and the adaptive
        let a = c.add_op(OpKind::Adaptive, t);
        c.op_mut(id).unwrap().suppressed = true;
        {
            let o = c.op_mut(a).unwrap();
            o.bottom = Some(-6.0);
            o.stepdown = Some(6.0);
        }
        let tp = generate(&c, &part).into_iter().find(|p| p.op == a).unwrap();
        let (climb, conv) = climb_and_conventional(&c, &part, &tp, top - 1e-6);
        assert!(climb > 500.0 && conv < 0.02 * climb, "adaptive: {climb:.0} mm climb, {conv:.0} mm conventional");
        // a pocket climbs too
        let part = plate();
        for kind in [OpKind::Pocket] {
            let (mut c, t) = setup();
            c.add_op(kind, t);
            let top = stock_box(&c, &part).1.z;
            let tp = generate(&c, &part).remove(0);
            let (climb, conv) = climb_and_conventional(&c, &part, &tp, top - 1e-6);
            assert!(climb > 20.0 && conv < 0.02 * climb, "{kind:?}: {climb:.0} mm climb, {conv:.0} mm conventional");
        }
    }

    #[test]
    fn adaptive_roughing_runs_at_its_power_target_with_the_peaks_capped() {
        // the plinth from an 86 mm square block, 6 mm levels at S9000 F3000 on the DMC2's spindle (2600 W, 43 %)
        let part = plinth();
        let mut c = Cam::default();
        c.stock.size = Some([86.0, 86.0, 30.0]);
        c.stock.bottom = 0.0;
        c.stock.material = "6061-T6".into();
        c.machine.spindle_power = 2600.0;
        c.machine.spindle_efficiency = 0.43;
        c.machine.spindle_rated_rpm = 9000.0;
        c.machine.max_feed = 3000.0;
        let mut tool = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.35).unwrap();
        (tool.flutes, tool.rpm, tool.feed, tool.flute_length, tool.stickout) = (3, 9000.0, 3000.0, 19.05, 24.0);
        let t = c.add_tool(tool);
        let a = c.add_op(OpKind::Adaptive, t);
        {
            let o = c.op_mut(a).unwrap();
            o.bottom = Some(-12.0);
            o.stepdown = Some(6.0);
        }
        let plain = generate(&c, &part).remove(0);
        c.op_mut(a).unwrap().power_target = Some(450.0);
        c.op_mut(a).unwrap().power_cap = Some(1.3 * 450.0);
        let tp = generate(&c, &part).remove(0);
        let note = tp.warnings.iter().find(|w| w.starts_with("power budget")).unwrap_or_else(|| panic!("{:?}", tp.warnings));
        let num = |key: &str| -> f64 {
            let i = note.find(key).unwrap();
            note[..i].trim_end().rsplit(|c: char| !(c.is_ascii_digit() || c == '.')).next().unwrap().parse().unwrap()
        };
        let (mean, peak) = (num(" W mean"), num(" W at the most"));
        assert!(peak <= 1.3 * 450.0 + 1.0, "{note}");
        assert!(mean > 0.5 * 450.0 && mean <= 1.3 * 450.0, "{note}");
        // more removed per minute than the default's narrow stepover, so less time
        assert!(tp.time < plain.time, "{} s against {} s; {note}", tp.time, plain.time);
        // the defaults from the spindle as measured (F004 = 150): the caution at the op's speed caps it, never
        // over the never-exceed; target 0 takes 60 % of what the spindle has there
        c.machine.spindle_above_rated = 0.76;
        c.machine.spindle_never_exceed = 0.85;
        c.machine.spindle_caution = 0.76;
        let op = c.op_mut(a).unwrap();
        (op.power_target, op.power_cap) = (Some(0.0), None);
        let budget = |c: &Cam| power_budget(c, c.op(a).unwrap(), &c.path_tool(a, t).unwrap()).unwrap();
        let (target, cap) = budget(&c);
        assert!((cap - 850.0).abs() < 2.0 && (target - 671.0).abs() < 2.0, "{target} {cap}");
        c.op_mut(a).unwrap().rpm = Some(12000.0);
        let (target, cap) = budget(&c);
        assert!((cap - 683.0).abs() < 2.0 && target < cap, "{target} {cap}");
        c.op_mut(a).unwrap().power_cap = Some(2000.0);
        let (_, cap) = budget(&c);
        assert!((cap - 765.0).abs() < 2.0, "never over the never-exceed: {cap}");
    }

    /// The feed schedule on the plinth (the DMC2 at S9000, F3000 top feed, 1118 W at the shaft, never past 85 %,
    /// caution 76 %: a 650 W target): adaptive and level-by-level clearing both come out faster, no load sample
    /// over the target by more than 5 %, nothing over a limit, and no rapid into stock or gouge.
    #[test]
    fn feed_schedule_holds_the_target_and_saves_time() {
        let part = plinth();
        for kind in [OpKind::Adaptive, OpKind::Clear] {
            let mut c = Cam::default();
            c.stock.size = Some([86.0, 86.0, 30.0]);
            c.stock.bottom = 0.0;
            c.stock.material = "6061-T6".into();
            c.machine.spindle_power = 2600.0;
            c.machine.spindle_efficiency = 0.43;
            c.machine.spindle_rated_rpm = 9000.0;
            c.machine.spindle_never_exceed = 0.85;
            c.machine.spindle_caution = 0.76;
            c.machine.max_feed = 3000.0;
            let mut tool = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.35).unwrap();
            (tool.flutes, tool.rpm, tool.feed, tool.flute_length, tool.stickout) = (3, 9000.0, 1500.0, 19.05, 24.0);
            let t = c.add_tool(tool);
            let a = c.add_op(kind, t);
            {
                let o = c.op_mut(a).unwrap();
                o.bottom = Some(-12.0);
                o.stepdown = Some(6.0);
            }
            let plain = generate(&c, &part).remove(0);
            c.op_mut(a).unwrap().feed_schedule = Some(true);
            let target = crate::cam_feed_schedule::default_target(&c, 9000.0);
            assert!((target - 671.0).abs() < 10.0, "{target}");
            let tp = generate(&c, &part).remove(0);
            let r = tp.feed_report.unwrap_or_else(|| panic!("{kind:?}: no report; {:?}", tp.warnings));
            println!("{kind:?}: {r:?}");
            assert!(r.peak_after_w <= 1.05 * target, "{kind:?}: {r:?}");
            assert!(tp.time < plain.time, "{kind:?}: {} s against {} s; {r:?}", tp.time, plain.time);
            assert!(r.mean_after_w > r.mean_before_w, "{kind:?}: {r:?}");
            // the moves as they run: every sample under the target (+5 %), no event at all
            let stock = stock_box(&c, &part);
            for res in [stock_resolution(stock), 300, 400, 500, 700] {
                let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, std::slice::from_ref(&tp), res);
                sim.profile = Some(vec![]);
                sim.run_to(usize::MAX, usize::MAX);
                let prof = sim.profile.as_ref().unwrap();
                let top = prof.iter().fold(None::<&crate::cam_sim::LoadSample>, |a, s| if a.is_none_or(|a| s.power > a.power) { Some(s) } else { a }).unwrap();
                println!("{kind:?} grid {res}: peak {} W {:?}", top.power.round(), top);
                // on the schedule's own grid within 5 % of the target; on any other a cell's difference in where
                // the stock's edge falls, still under the cap (the spindle's caution)
                let own = res == stock_resolution(stock);
                assert!(top.power <= if own { 1.05 * target } else { r.cap_w }, "{kind:?} grid {res}: {top:?}");
                assert!(sim.events.is_empty(), "{kind:?}: {:?}", sim.events.iter().take(5).collect::<Vec<_>>());
            }
            // the same path: same moves where it cuts, only the feeds differ (and long moves split, then joined)
            let ends = |t: &Toolpath| t.moves.iter().filter(|m| matches!(m, Move::Rapid(_))).count();
            assert_eq!(ends(&tp), ends(&plain), "{kind:?}");
        }
    }

    /// Distance from the plinth's outline (outside it).
    fn plinth_distance(p: DVec2) -> f64 {
        let q = p.abs() - DVec2::splat(27.0);
        q.max(DVec2::ZERO).length() - 10.0
    }

    /// Phone stand op 1 (rcnc jobs/phone-stand/op1-underside.nc): 76 × 76 stock round the 74 × 74 R10 plinth,
    /// Ø6.35 flat end mill, 4 mm steps to 14.5 deep leaving 0.2 for a finishing pass. The wall pass alone takes
    /// 1 mm along the sides but about 5.6 mm at the corners; the extra passes must keep every corner under the limit.
    #[test]
    fn contour_roughing_keeps_corner_engagement_under_the_limit() {
        let part = plinth();
        let mut c = Cam::default();
        c.stock.size = Some([76.0, 76.0, 30.0]);
        c.stock.bottom = 0.0;
        let mut tool = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.35).unwrap();
        tool.flutes = 3;
        tool.rpm = 24000.0;
        tool.feed = 2400.0;
        tool.flute_length = 19.05;
        let t = c.add_tool(tool);
        let id = c.add_op(OpKind::Contour, t);
        // ramping (the lead arcs are checked further down)
        c.op_mut(id).unwrap().lead = 0.0;
        {
            let o = c.op_mut(id).unwrap();
            o.side = Side::Outside;
            o.bottom = Some(-14.5);
            o.stepdown = Some(4.0);
            o.leave = 0.2;
        }
        let ctx = Ctx::new(&c, &part);
        let top = ctx.stock.1.z;
        let material = difference(&rect(ctx.stock.0, ctx.stock.1), &offset(&ctx.geo.shadow(top - 1.0), 0.2));
        let r = 6.35 / 2.0;
        // radial engagement on a 0.1 mm grid of the block (the spread of the cells each step takes, never more than the true width)
        let measure = |tp: &Toolpath| crate::cam_contour::check(&tp.moves, None, &material, top, r, 0.1).unwrap();

        // the old single wall pass (no limit, passes a whole diameter apart): buried at the corners
        c.op_mut(id).unwrap().max_engagement = Some(1.0);
        c.op_mut(id).unwrap().stepover = Some(1.0);
        let single = generate(&c, &part).remove(0);
        let e = measure(&single);
        assert!(e.peak > 4.0 && e.at.x.abs() > 27.0 && e.at.y.abs() > 27.0, "the wall pass alone should take about 5.6 mm at a corner: {e:?}");
        let d = |p: &DVec3| plinth_distance(p.truncate());
        assert!(feeds(&single).iter().filter(|p| p.z < top - 1e-6).all(|p| d(p) < r + 0.2 + 0.05), "no extra passes without a limit");

        // the default: 25 % of the diameter, everywhere (corners, ramps and all)
        c.op_mut(id).unwrap().max_engagement = None;
        c.op_mut(id).unwrap().stepover = None;
        let limit = 0.25 * 6.35;
        let tp = generate(&c, &part).remove(0);
        assert!(tp.warnings.iter().all(|w| !w.contains("radially")), "{:?}", tp.warnings);
        let e = measure(&tp);
        assert!(e.peak <= limit, "peak radial engagement {} mm at {} is over the {limit} mm limit", e.peak, e.at);
        // arcs onto the wall instead of ramping along it keep under the limit too
        c.op_mut(id).unwrap().lead = 3.0;
        let led = generate(&c, &part).remove(0);
        let e = measure(&led);
        assert!(e.peak <= limit, "with lead arcs: {} mm at {}", e.peak, e.at);
        assert!(led.warnings.iter().all(|w| !w.contains("radially")), "{:?}", led.warnings);
        c.op_mut(id).unwrap().lead = 0.0;
        // the extra passes run only round the corners: along the sides they would be in air
        let fs = feeds(&tp);
        let out: Vec<&DVec3> = fs.iter().filter(|p| p.z < top - 1e-6 && d(p) > r + 0.2 + 0.5).collect();
        assert!(!out.is_empty(), "no extra passes");
        for p in &out {
            assert!(p.x.abs() > 18.0 && p.y.abs() > 18.0, "extra pass in air along a side at {p}");
        }
        // every pass gets down to 14.5 below the top
        let low = fs.iter().map(|p| p.z).fold(f64::INFINITY, f64::min);
        assert!((low - (top - 14.5)).abs() < 1e-6, "lowest {low}");
        // the simulation agrees: nothing left outside the 0.2 mm leave down to the bottom of the cut
        let sim = final_stock(&c, &part, std::slice::from_ref(&tp), None);
        for (x, y) in [(37.9, 37.9), (-37.9, 37.9), (36.0, 37.5), (0.0, 37.9), (37.9, 0.0)] {
            let left = sim.top(x, y) as f64;
            assert!(left <= top - 14.5 + 0.3, "stock left at ({x}, {y}) up to {left}");
        }

        // a part cut out of a much bigger block: the contour can't keep its engagement down on its own, and says so
        c.stock.size = Some([150.0, 150.0, 30.0]);
        let big = generate(&c, &part).remove(0);
        assert!(big.warnings.iter().any(|w| w.contains("radially") && w.contains("adaptive")), "{:?}", big.warnings);
    }
}
