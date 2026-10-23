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

use crate::cam::{Cam, METRIC_COARSE, OpKind, Operation, Origin, Side, Tool, ToolKind};
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

/// A circular edge: centre, axis, radius.
pub type Circle = ([f64; 3], [f64; 3], f64);

/// The parts being machined, in setup coordinates.
pub struct Part {
    pub tris: Vec<[DVec3; 3]>,
    pub circles: Vec<(DVec3, DVec3, f64)>,
    pub lo: DVec3,
    pub hi: DVec3,
    /// Part → setup rotation.
    pub rot: DMat3,
}

impl Part {
    pub fn new(tris: &[[DVec3; 3]], circles: &[Circle], orient: &str) -> Part {
        let rot = orient_matrix(orient);
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
        Part { tris, circles, lo, hi, rot }
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

/// The work origin (G54 zero) in setup coordinates.
pub fn work_origin(cam: &Cam, part: &Part) -> DVec3 {
    let (lo, hi) = stock_box(cam, part);
    match cam.origin {
        Origin::StockTopCorner => DVec3::new(lo.x, lo.y, hi.z),
        Origin::StockTopCenter => DVec3::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, hi.z),
        Origin::StockBottomCorner => lo,
        Origin::PartTopCorner => DVec3::new(part.lo.x, part.lo.y, part.hi.z),
        Origin::Model => DVec3::ZERO,
    }
}

// ---------------------------------------------------------------- 2D helpers

pub(crate) fn flat(s: &Shapes) -> Vec<Contour> {
    s.iter().flat_map(|sh| sh.iter().cloned()).collect()
}

pub fn union_all(contours: &[Contour]) -> Shapes {
    if contours.is_empty() {
        return vec![];
    }
    let mut ov = FloatOverlay::with_subj(contours);
    clean(ov.overlay(OverlayRule::Subject, FillRule::NonZero))
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
            // too tight to ramp: plunge
            self.cut(DVec3::new(s0.x, s0.y, z), self.plunge);
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

    /// Clear the tool-centre area `area` at height z with offset rings, spiralling out from the middle of each region.
    fn clear_area(&mut self, area: &Shapes, z: f64, z_from: f64, step: f64, climb: bool) {
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
            let mut rings: Vec<Vec<Contour>> = vec![];
            let mut cur = region.clone();
            while !cur.is_empty() && rings.len() < 5000 {
                rings.push(flat(&cur));
                cur = offset(&cur, -step);
            }
            let link_region = offset(&region, 0.01);
            let mut first = true;
            for ring in rings.iter().rev() {
                let mut loops: Vec<Contour> = ring.iter().map(|c| if climb { c.clone() } else { c.iter().rev().copied().collect() }).collect();
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
                    let from = if first { z_from } else { z };
                    // already at depth inside the region: link straight over, else ramp in from above
                    if !first && self.pos.map(|p| (p.z - z).abs() < 1e-6).unwrap_or(false) {
                        let before = self.moves.len();
                        self.cut_loop(&l, z, z, step * 1.6, Some(&link_region), None);
                        // a retract happened: the loop was entered from above, so it must ramp
                        if self.moves[before..].iter().any(|m| matches!(m, Move::Rapid(_))) {
                            self.moves.truncate(before);
                            self.pos = recompute_pos(&self.moves);
                            self.cut_loop(&l, z, z_from, 0.0, None, None);
                        }
                    } else {
                        self.cut_loop(&l, z, from, 0.0, None, None);
                    }
                    first = false;
                }
            }
        }
    }
}

fn recompute_pos(moves: &[Move]) -> Option<DVec3> {
    let mut p: Option<DVec3> = None;
    for m in moves {
        p = Some(m.end(p.unwrap_or(DVec3::ZERO)));
    }
    p
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

    fn part(&self) -> &Part {
        self.geo.part
    }

    pub(crate) fn wz(&self, z: Option<f64>) -> Option<f64> {
        z.map(|z| z + self.origin.z)
    }

    fn safe(&self) -> f64 {
        self.stock.1.z.max(self.part().hi.z) + self.cam.clearance
    }
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
    let mut ctx = Ctx::new(cam, part);
    let ops: Vec<&Operation> = cam.ops.iter().filter(|o| !o.suppressed).collect();
    let needs = |o: &Operation| (o.rest || stock.is_some()) && o.kind.uses_stock();
    let last = ops.iter().rposition(|o| needs(o));
    let mut sim: Option<Sim> = None;
    let mut out: Vec<Toolpath> = vec![];
    for (k, op) in ops.iter().enumerate() {
        ctx.stock_in = None;
        if needs(op) {
            let s = sim.get_or_insert_with(|| {
                let mut s = Sim::new(cam, part, ctx.stock, &[], stock_resolution(ctx.stock));
                if let Some(m) = stock {
                    s.start_from(m);
                }
                for tp in &out {
                    s.push(cam, tp);
                }
                s
            });
            s.run_to(usize::MAX, usize::MAX);
            ctx.stock_in = Some(s.stock_map());
        }
        let tp = op_path(&ctx, op);
        if let Some(s) = sim.as_mut() {
            if last.map(|l| k < l).unwrap_or(false) {
                s.push(cam, &tp);
            }
        }
        out.push(tp);
    }
    machine_checks(cam, &ctx, &mut out);
    out
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
            tp.warnings.push(format!("T{} runs at {} rpm but the spindle tops out at {}", tool.number, tool.rpm.round(), cam.max_rpm.round()));
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
pub fn link_setups(cams: &mut [Cam], ci: usize, tris: &[[DVec3; 3]], circles: &[Circle]) -> Result<Option<StockMap>, String> {
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
        let pa = Part::new(tris, circles, &cams[a].orient);
        let (lo, hi) = stock_box(&cams[a], &pa);
        let (p, q) = (pa.to_part(lo), pa.to_part(hi));
        cams[b].stock.carried = Some([p.min(q).to_array(), p.max(q).to_array()]);
        let paths = generate_from(&cams[a], &pa, stock.as_ref());
        let left = final_stock(&cams[a], &pa, &paths, stock.as_ref());
        stock = Some(left.reorient(pa.rot, orient_matrix(&cams[b].orient)));
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
    let mut pb = Pb {
        tool,
        moves: vec![],
        pos: None,
        safe: ctx.safe(),
        retract: ctx.cam.retract.max(0.1),
        ramp: op.ramp_angle.clamp(0.5, 90.0).to_radians(),
        feed: tool.feed.max(1.0),
        plunge: tool.plunge_feed.max(1.0),
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
    }
    pb.up();
    // deepest cut below the stock top against the flute length
    if !tool.kind.is_hole_tool() && !matches!(tool.kind, ToolKind::Slot | ToolKind::ThreadMill) {
        let top = ctx.stock.1.z;
        let low = pb.moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(p[2]) } else { None }).fold(f64::INFINITY, f64::min);
        if low.is_finite() && top - low > tool.flute_length + 1e-6 {
            tp.warnings.push(format!("cuts {} mm deep but the flutes are only {} mm long", crate::cam::fmt_mm(top - low), crate::cam::fmt_mm(tool.flute_length)));
        }
    }
    tp.moves = pb.moves;
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
    let bound = offset(&rect(lo, hi), r * 0.5);
    let floors: Vec<f64> = ctx.geo.floors().iter().map(|f| f + op.leave_floor).collect();
    let mut z_prev = top;
    let mut any = false;
    for z in levels(top, bottom, stepdown(op, t), &floors) {
        let keep = offset(&ctx.geo.shadow(z), r + op.leave);
        let area = rest_area(ctx, difference(&bound, &keep), z, r, op.leave);
        if area.is_empty() {
            z_prev = z;
            continue;
        }
        any = true;
        pb.clear_area(&area, z, z_prev, stepover(op, t), op.climb);
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
    let mut reg = enclosed(&ctx.geo.shadow(z));
    reg.retain(|sh| ctx.geo.floor_pts.iter().any(|f| f.z <= z + 1e-3 && shape_inside(sh, [f.x, f.y])));
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
            z_prev = z;
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
        pb.clear_area(&area, z, z_prev, stepover(op, t), op.climb);
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
    let mut z_prev = top;
    let mut any = false;
    for z in lv {
        let sh = ctx.geo.shadow(z);
        let path = offset(&sh, off);
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
            z_prev = z;
            continue;
        }
        any = true;
        let tabs = (op.tabs > 0 && z < tab_top - 1e-6).then_some((op.tabs, op.tab_width, tab_top));
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
            } else {
                pb.cut_loop(&l, z, z_prev, 0.05, None, tabs);
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
}

fn chamfer(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let t = pb.tool;
    let edge = ctx.wz(op.top).unwrap_or(ctx.part().hi.z);
    let half = (t.angle.max(10.0) / 2.0).to_radians();
    let tip = if t.kind == ToolKind::Chamfer { t.tip_diameter / 2.0 } else { 0.0 };
    let e = 0.3_f64;
    let w = op.chamfer.max(0.0);
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
                pb.cut_loop(&l, z, z, 0.0, None, None);
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
                pb.approach(h.x, h.y, top);
                if op.peck > 0.0 {
                    let mut cur = top;
                    while cur > bottom + 1e-6 {
                        let next = (cur - op.peck).max(bottom);
                        if cur < top - 1e-6 {
                            pb.rapid(DVec3::new(h.x, h.y, cur + 0.3));
                        }
                        pb.cut(DVec3::new(h.x, h.y, next), pb.feed);
                        cur = next;
                        if cur > bottom + 1e-6 {
                            pb.rapid(DVec3::new(h.x, h.y, rz));
                        }
                    }
                } else {
                    pb.cut(DVec3::new(h.x, h.y, bottom), pb.feed);
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
                pb.approach(h.x + rh, h.y, top);
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
                pb.approach(h.x, h.y, top);
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
                pb.approach(h.x, h.y, top);
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
                let l = 2.0 * (r - z).abs();
                cut += l;
                time += l / (tool.rpm.max(1.0) * pitch.max(0.01)) * 60.0 + 1.0;
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
        assert!(sim.events.is_empty(), "{:?}", sim.events);
        assert!(sim.removed > 1000.0);
        // a rapid straight through the stock is a crash
        let bad = Toolpath { op: 1, tool: t, moves: vec![Move::Rapid([-10.0, 20.0, 5.0]), Move::Rapid([70.0, 20.0, 5.0])], ..Default::default() };
        let mut sim = crate::cam_sim::Sim::new(&c, &part, stock, &[bad], 300);
        sim.run_to(usize::MAX, usize::MAX);
        assert!(sim.events.iter().any(|e| e.kind == crate::cam_sim::EventKind::Rapid));
        assert!(sim.events.iter().any(|e| e.kind == crate::cam_sim::EventKind::Gouge));
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
        for post in ["grbl", "linuxcnc", "fanuc", "mach3"] {
            c.post = post.into();
            let g = crate::cam_post::gcode(&c, &paths, origin, top + c.clearance);
            assert!(g.contains("G21") && g.contains("M30"), "{post}");
            assert_eq!(g.contains("T1 M6"), post != "grbl", "{post}");
            // work zero at the stock top: nothing cuts above Z0
            for l in g.lines().filter(|l| l.starts_with("G1") || l.starts_with('Z')) {
                if let Some(z) = l.split_whitespace().find(|w| w.starts_with('Z')) {
                    let z: f64 = z[1..].parse().unwrap();
                    assert!(z <= c.clearance + 1e-6, "{l}");
                }
            }
        }
    }
}
