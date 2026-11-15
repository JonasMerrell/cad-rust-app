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
            let mut lv = crate::cam_3d::Level { pb: &mut *self, z, z_from, r, step, feed, fast: feed, climb, helix: 0.4 * 2.0 * r };
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
    generate_all(cam, part, stock, false).0
}

/// The toolpaths and the stock once they have all run.
pub fn generate_with_stock(cam: &Cam, part: &Part, stock: Option<&StockMap>) -> (Vec<Toolpath>, StockMap) {
    let (paths, left) = generate_all(cam, part, stock, true);
    let left = left.unwrap_or_else(|| final_stock(cam, part, &paths, stock));
    (paths, left)
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
        let Some(tool) = cam.tool(tp.tool) else { continue };
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
/// and return the stock the earlier setups leave for it (None for a first setup).
pub fn link_setups(cams: &mut [Cam], ci: usize, input: &PartInput) -> Result<Option<StockMap>, String> {
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
        let pa = Part::from_input(input, &cams[a]);
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
    let Some(tool) = ctx.cam.tool(op.tool) else {
        tp.warnings.push("no tool selected".into());
        return tp;
    };
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
            let (m, n) = avoid_fixtures(moves, &lowered, r, ctx.cam.retract.max(1.0), start);
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
    let x0 = lo.x - r - 1.0;
    let x1 = hi.x + r + 1.0;
    let w = hi.y - lo.y;
    let n = (w / step).ceil().max(1.0) as usize;
    let mut z_prev = top;
    for z in levels(top, bottom, stepdown(op, t), &[]) {
        for i in 0..=n {
            let y = lo.y + w * i as f64 / n as f64;
            let (a, b) = if i % 2 == 0 { (x0, x1) } else { (x1, x0) };
            if i == 0 {
                pb.approach(a, y, z_prev);
                pb.cut(DVec3::new(a, y, z), pb.plunge);
            } else {
                pb.cut(DVec3::new(a, y, z), pb.feed);
            }
            pb.cut(DVec3::new(b, y, z), pb.feed);
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
        let mut loops: Vec<Contour> = vec![];
        for s in &path {
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
                        outer_pass(pb, &pass, &sh, &outside, z, z_prev, r, step, op.lead);
                    }
                }
                Err(reach) => too_far = Some(too_far.unwrap_or(0.0).max(reach)),
            }
        }
        // what is left for the wall pass: what the extra passes didn't take
        let wall_left = if outer_ok { crate::cam_3d::intersect(&material, &offset(&sh, op.leave + step)) } else { material.clone() };
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
            } else if op.lead <= 0.0 || !lead_loop(pb, &l, z, z_prev, &offset(&sh, r * 0.98), op.lead, tabs, Some((&wall_left, limit))) {
                pb.cut_loop(&l, z, z_prev, 0.05, None, tabs);
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
fn outer_pass(pb: &mut Pb, pass: &crate::cam_contour::Pass, sh: &Shapes, material: &Shapes, z: f64, z_from: f64, r: f64, step: f64, lead: f64) {
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
            if lead <= 0.0 || !lead_loop(pb, &s.pts, z, z_from, &offset(sh, pass.dist - 0.02 * r), lead, None, Some((&left, step))) {
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

/// Cut a closed loop at `z` with arcs of radius `lead` onto and off it, on the side away from the material
/// (`keep_out`: where the tool's centre mustn't go), starting mid-way along a side. False when there is no room.
/// `guard`: the material there and the most the tool may take radially; a lead-in that would take more (arcing
/// into stock left on the wall) is passed over for another start, and with none left the caller ramps instead.
fn lead_loop(pb: &mut Pb, l: &[P2], z: f64, z_from: f64, keep_out: &Shapes, lead: f64, tabs: Option<(u32, f64, f64)>, guard: Option<(&Shapes, f64)>) -> bool {
    let n = l.len();
    if n < 3 {
        return false;
    }
    let here = pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
    // the middles of the sides, nearest first
    let mut starts: Vec<usize> = (0..n).filter(|i| DVec2::from(l[*i]).distance(DVec2::from(l[(i + 1) % n])) > 1e-3).collect();
    let mid = |i: usize| (DVec2::from(l[i]) + DVec2::from(l[(i + 1) % n])) / 2.0;
    starts.sort_by(|a, b| mid(*a).distance_squared(here).total_cmp(&mid(*b).distance_squared(here)));
    let steps = 12;
    for &i in starts.iter().take(24) {
        let s0 = mid(i);
        let t = (DVec2::from(l[(i + 1) % n]) - DVec2::from(l[i])).normalize();
        for side in [1.0, -1.0] {
            // centre beside the start; in along a quarter circle ending tangent to the loop, out along another
            let u = -t.perp() * side;
            let c = s0 - u * lead;
            let quarter = |f: &dyn Fn(f64) -> DVec2| (0..=steps).map(|k| f(k as f64 / steps as f64 * std::f64::consts::FRAC_PI_2)).collect::<Vec<DVec2>>();
            let lead_in = quarter(&|a: f64| c + (u * a.sin() - t * a.cos()) * lead);
            let lead_out = quarter(&|a: f64| c + (u * a.cos() + t * a.sin()) * lead);
            if lead_in.iter().chain(&lead_out).any(|p| inside(keep_out, [p.x, p.y])) {
                continue;
            }
            if let Some((material, limit)) = guard {
                let r = pb.tool.radius();
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
                // ramp down back and forth along the lead-in arc (clear of the part) instead of plunging into the stock
                pb.cut(lead_in[0].extend(z_from), pb.plunge);
                let len: f64 = lead_in.windows(2).map(|w| w[0].distance(w[1])).sum();
                let per_pass = 2.0 * len * pb.ramp.tan().max(1e-3);
                let pairs = ((dz / per_pass).ceil() as usize).clamp(1, 1000);
                let step = dz / (2 * pairs) as f64;
                let mut zz = z_from;
                for _ in 0..pairs {
                    for path in [lead_in.iter().collect::<Vec<_>>(), lead_in.iter().rev().collect::<Vec<_>>()] {
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
            for p in &lead_out[1..] {
                pb.cut(p.extend(z), pb.feed);
            }
            return true;
        }
    }
    false
}

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
        let Some(t) = cam.tool(tp.tool) else { continue };
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
        // quarter circle of radius 3 at depth arriving at the wall offset (3 mm in)
        let i = (1..pts.len() - 13)
            .find(|&i| pts[i].truncate().distance(pts[0].truncate()) < 1e-9 && pts[i + 1..i + 13].iter().all(|p| (p.z - pts[i].z).abs() < 1e-9))
            .unwrap();
        assert!(pts[i].z < pts[0].z - 1.0);
        assert!(pts[..=i].windows(2).all(|w| w[0].z - w[1].z <= 0.5 * w[0].truncate().distance(w[1].truncate()) + 1e-9), "plunges");
        let lead: Vec<DVec3> = pts[i..i + 13].to_vec();
        let end = lead[12];
        let wall = (end.x - 10.0).min(30.0 - end.x).min(end.y - 10.0).min(30.0 - end.y);
        assert!((wall - 3.0).abs() < 0.02, "{end}");
        let chord = lead[0].truncate().distance(end.truncate());
        assert!((chord - 3.0 * 2f64.sqrt()).abs() < 0.05, "{chord}");
        assert!(lead.iter().all(|p| (p.z - lead[0].z).abs() < 1e-9));
        // and it ends on the way out, off the wall again
        let last = *pts.last().unwrap();
        let wall = (last.x - 10.0).min(30.0 - last.x).min(last.y - 10.0).min(30.0 - last.y);
        assert!(wall > 4.0, "{last}");
        // no room for the lead: back to ramping
        c.op_mut(id).unwrap().lead = 20.0;
        let ramped = generate(&c, &part).remove(0);
        assert!(feeds(&ramped).windows(2).any(|w| w[1].z < w[0].z - 1e-6 && w[0].truncate().distance(w[1].truncate()) > 1e-6), "ramps along the wall");
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
