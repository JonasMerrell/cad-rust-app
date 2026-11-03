//! Sketch inference while drawing: alignment with existing points, a weak grid,
//! and the direction of a line being drawn (horizontal, vertical, parallel or
//! perpendicular to a line), Onshape style. Pure geometry so it can be tested
//! without the UI.

use std::collections::HashSet;

use glam::DVec2;

use crate::sketch::{Constraint, Geom, Id, Sketch};

/// How far (screen pixels) the cursor may be from a grid line before it snaps to it.
/// Small, so the grid helps without fighting the hand.
pub const GRID_PX: f64 = 3.0;
/// How far (pixels) the cursor may be from lining up with a point.
pub const ALIGN_PX: f64 = 6.0;
/// How far (pixels) the cursor may be off an inferred direction.
pub const DIR_PX: f64 = 8.0;
/// The most an inferred direction may turn the line (degrees).
pub const DIR_DEG: f64 = 5.0;

/// The direction a line being drawn follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Horizontal,
    Vertical,
    Parallel(Id),
    Perpendicular(Id),
}

impl Dir {
    /// The constraint this direction adds to new line `l`.
    pub fn constraint(self, l: Id) -> Constraint {
        match self {
            Dir::Horizontal => Constraint::Horizontal { l },
            Dir::Vertical => Constraint::Vertical { l },
            Dir::Parallel(o) => Constraint::Parallel { a: o, b: l },
            Dir::Perpendicular(o) => Constraint::Perpendicular { a: o, b: l },
        }
    }

    /// The other line this direction refers to.
    pub fn reference(self) -> Option<Id> {
        match self {
            Dir::Parallel(o) | Dir::Perpendicular(o) => Some(o),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inferred {
    /// The cursor after inference.
    pub p: DVec2,
    /// The direction a line being drawn was locked to.
    pub dir: Option<Dir>,
    /// Points the cursor lines up with (a dashed guide is drawn from each).
    pub align: Vec<DVec2>,
    /// Some coordinate snapped to the grid.
    pub grid: bool,
}

/// Points that can be lined up with: every sketch point not excluded, and the origin.
fn align_points(sk: &Sketch, exclude: &HashSet<Id>) -> Vec<DVec2> {
    let mut v: Vec<DVec2> = sk.points.iter().filter(|p| !exclude.contains(&p.id)).map(|p| p.v()).collect();
    v.push(DVec2::ZERO);
    v
}

/// The nearest point lining up with `raw` along x (same x, vertical guide) and along y.
fn nearest_align(pts: &[DVec2], raw: DVec2, tol: f64) -> (Option<DVec2>, Option<DVec2>) {
    let mut bx: Option<(f64, DVec2)> = None;
    let mut by: Option<(f64, DVec2)> = None;
    for &q in pts {
        // a point right under the cursor is a point snap's job, not a guide
        if (q - raw).length() < tol {
            continue;
        }
        let dx = (q.x - raw.x).abs();
        if dx < tol && bx.map(|b| dx < b.0 - 1e-12 || (dx - b.0).abs() <= 1e-12 && (q - raw).length() < (b.1 - raw).length()).unwrap_or(true) {
            bx = Some((dx, q));
        }
        let dy = (q.y - raw.y).abs();
        if dy < tol && by.map(|b| dy < b.0 - 1e-12 || (dy - b.0).abs() <= 1e-12 && (q - raw).length() < (b.1 - raw).length()).unwrap_or(true) {
            by = Some((dy, q));
        }
    }
    (bx.map(|b| b.1), by.map(|b| b.1))
}

fn grid_axis(v: f64, g: f64, tol: f64) -> Option<f64> {
    let s = (v / g).round() * g;
    ((s - v).abs() < tol).then_some(s)
}

/// A free cursor (nothing under it): line up with points, else snap each axis to the
/// grid when it is within a few pixels of a grid line. `wpp` is sketch units per pixel.
pub fn free_point(sk: &Sketch, raw: DVec2, wpp: f64, grid: Option<f64>, exclude: &HashSet<Id>) -> Inferred {
    let pts = align_points(sk, exclude);
    let (ax, ay) = nearest_align(&pts, raw, ALIGN_PX * wpp);
    let mut out = Inferred { p: raw, ..Default::default() };
    match ax {
        Some(q) => {
            out.p.x = q.x;
            out.align.push(q);
        }
        None => {
            if let Some(x) = grid.and_then(|g| grid_axis(raw.x, g, GRID_PX * wpp)) {
                out.p.x = x;
                out.grid = true;
            }
        }
    }
    match ay {
        Some(q) => {
            out.p.y = q.y;
            if !out.align.contains(&q) {
                out.align.push(q);
            }
        }
        None => {
            if let Some(y) = grid.and_then(|g| grid_axis(raw.y, g, GRID_PX * wpp)) {
                out.p.y = y;
                out.grid = true;
            }
        }
    }
    out
}

/// Lines a new line from `anchor` may be parallel or perpendicular to: the lines ending at
/// the anchor (usually the previous segment) and lines the cursor recently passed over.
pub fn candidate_lines(sk: &Sketch, anchor: Id, woken: &[Id]) -> Vec<Id> {
    let mut out: Vec<Id> = vec![];
    for c in sk.curves.iter().rev() {
        if let Geom::Line { a, b } = c.geom {
            if a == anchor || b == anchor {
                out.push(c.id);
            }
        }
    }
    for id in woken {
        if !out.contains(id) && matches!(sk.curve(*id).map(|c| &c.geom), Some(Geom::Line { .. })) {
            out.push(*id);
        }
    }
    out
}

/// The end of a line being drawn from `anchor`: lock it horizontal, vertical, or parallel /
/// perpendicular to a candidate line when the cursor is close to that direction, then line
/// the free end up with a point (or the weak grid) along that direction.
pub fn line_end(sk: &Sketch, anchor: Id, raw: DVec2, wpp: f64, grid: Option<f64>, woken: &[Id], exclude: &HashSet<Id>) -> Inferred {
    let a = sk.pv(anchor);
    let d = raw - a;
    let len = d.length();
    let mut ex = exclude.clone();
    ex.insert(anchor);
    // too short to have a direction yet
    if len < 4.0 * DIR_PX * wpp {
        return free_point(sk, raw, wpp, grid, &ex);
    }
    let mut dirs: Vec<(Dir, DVec2)> = vec![(Dir::Horizontal, DVec2::X), (Dir::Vertical, DVec2::Y)];
    for l in candidate_lines(sk, anchor, woken) {
        let Some(Geom::Line { a: p, b: q }) = sk.curve(l).map(|c| c.geom.clone()) else { continue };
        let u = (sk.pv(q) - sk.pv(p)).normalize_or_zero();
        if u == DVec2::ZERO {
            continue;
        }
        // a horizontal or vertical line adds nothing over H / V
        if u.x.abs() < 1e-6 || u.y.abs() < 1e-6 {
            continue;
        }
        dirs.push((Dir::Parallel(l), u));
        dirs.push((Dir::Perpendicular(l), u.perp()));
    }
    let max_sin = DIR_DEG.to_radians().sin();
    let mut best: Option<(f64, Dir, DVec2)> = None;
    for (k, (dir, u)) in dirs.into_iter().enumerate() {
        // sign the direction toward the cursor
        let u = if u.dot(d) < 0.0 { -u } else { u };
        let off = u.perp_dot(d).abs();
        if off > DIR_PX * wpp || off / len > max_sin {
            continue;
        }
        // H and V win ties (and anything within a pixel), so a level line stays "horizontal"
        let score = if k < 2 { off - wpp } else { off };
        if best.map(|b| score < b.0).unwrap_or(true) {
            best = Some((score, dir, u));
        }
    }
    let Some((_, dir, u)) = best else { return free_point(sk, raw, wpp, grid, &ex) };
    let t = d.dot(u);
    let mut out = Inferred { p: a + u * t, dir: Some(dir), ..Default::default() };
    // along the locked direction: stop where the end lines up with a point
    let pts = align_points(sk, &ex);
    let tol = ALIGN_PX * wpp;
    let mut stop: Option<(f64, DVec2, DVec2)> = None;
    for q in pts {
        // where the guide through q (vertical, then horizontal) crosses the line
        for axis in [DVec2::Y, DVec2::X] {
            let den = u.perp_dot(axis);
            if den.abs() < 0.2 {
                continue; // guide nearly along the line
            }
            // a + u s lies on q + axis r
            let s = (q - a).perp_dot(axis) / u.perp_dot(axis);
            if s <= 0.0 {
                continue;
            }
            let hit = a + u * s;
            let dist = (s - t).abs();
            if dist < tol && (hit - q).length() > tol && stop.map(|b| dist < b.0).unwrap_or(true) {
                stop = Some((dist, hit, q));
            }
        }
    }
    if let Some((_, hit, q)) = stop {
        out.p = hit;
        out.align.push(q);
    } else if let Some(g) = grid {
        // a horizontal or vertical line can still land its free coordinate on the grid
        match dir {
            Dir::Horizontal => {
                if let Some(x) = grid_axis(out.p.x, g, GRID_PX * wpp) {
                    out.p.x = x;
                    out.grid = true;
                }
            }
            Dir::Vertical => {
                if let Some(y) = grid_axis(out.p.y, g, GRID_PX * wpp) {
                    out.p.y = y;
                    out.grid = true;
                }
            }
            _ => {}
        }
    }
    out
}

/// What a dragged point was dropped onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    Point(Id),
    Curve(Id),
    Mid(Id),
    Origin,
}

/// Join point `p` to what it was dropped onto: merge it into another point, or constrain it
/// onto a curve, a line's midpoint or the origin. False when that would make no sense (both
/// ends of one line, a curve's own end) or is already so.
pub fn drop_point(sk: &mut Sketch, p: Id, onto: Drop) -> bool {
    let has = |sk: &Sketch, k: &Constraint| {
        sk.constraints.iter().any(|c| match (c, k) {
            (Constraint::Midpoint { p: a, l: b }, Constraint::Midpoint { p: x, l: y })
            | (Constraint::PointOnLine { p: a, l: b }, Constraint::PointOnLine { p: x, l: y })
            | (Constraint::PointOnCircle { p: a, c: b }, Constraint::PointOnCircle { p: x, c: y }) => a == x && b == y,
            _ => false,
        })
    };
    match onto {
        Drop::Point(q) => {
            if q == p || !sk.is_point(q) {
                return false;
            }
            // two points of one curve would collapse it
            if sk.curves.iter().any(|c| {
                let pts = Sketch::curve_points(&c.geom);
                pts.contains(&p) && pts.contains(&q)
            }) {
                return false;
            }
            sk.merge_point(p, q);
            true
        }
        Drop::Curve(c) | Drop::Mid(c) => {
            let Some(curve) = sk.curve(c) else { return false };
            if Sketch::curve_points(&curve.geom).contains(&p) {
                return false;
            }
            let k = match (&curve.geom, onto) {
                (Geom::Line { .. }, Drop::Mid(_)) => Constraint::Midpoint { p, l: c },
                (Geom::Line { .. }, _) => Constraint::PointOnLine { p, l: c },
                (Geom::Circle { .. } | Geom::Arc { .. } | Geom::Ellipse { .. } | Geom::EllipticalArc { .. }, Drop::Curve(_)) => Constraint::PointOnCircle { p, c },
                _ => return false,
            };
            if has(sk, &k) {
                return false;
            }
            sk.constraints.push(k);
            true
        }
        Drop::Origin => {
            if sk.constraints.iter().any(|k| matches!(k, Constraint::Fix { p: q, .. } if *q == p)) {
                return false;
            }
            sk.constraints.push(Constraint::Fix { p, x: 0.0, y: 0.0 });
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk_with_line(a: DVec2, b: DVec2) -> (Sketch, Id, Id, Id) {
        let mut sk = Sketch::default();
        let pa = sk.add_point(a);
        let pb = sk.add_point(b);
        let l = sk.add_curve(Geom::Line { a: pa, b: pb }, false);
        (sk, pa, pb, l)
    }

    const WPP: f64 = 0.1; // 10 px per mm

    #[test]
    fn grid_is_weak() {
        let sk = Sketch::default();
        let none = HashSet::new();
        // 1.3 px off a grid line in x snaps, 4 px off in y doesn't
        let r = free_point(&sk, DVec2::new(10.13, 10.4), WPP, Some(1.0), &none);
        assert!((r.p.x - 10.0).abs() < 1e-9);
        assert!((r.p.y - 10.4).abs() < 1e-9);
        assert!(r.grid);
        // far from the grid lines: the cursor stays where it is
        let r = free_point(&sk, DVec2::new(10.45, 10.55), WPP, Some(1.0), &none);
        assert_eq!(r.p, DVec2::new(10.45, 10.55));
        assert!(!r.grid);
        // no grid
        let r = free_point(&sk, DVec2::new(10.13, 10.4), WPP, None, &none);
        assert_eq!(r.p, DVec2::new(10.13, 10.4));
    }

    #[test]
    fn points_beat_grid() {
        let mut sk = Sketch::default();
        sk.add_point(DVec2::new(3.27, 8.0));
        let r = free_point(&sk, DVec2::new(3.3, 20.02), WPP, Some(1.0), &HashSet::new());
        assert!((r.p.x - 3.27).abs() < 1e-9, "x lines up with the point, {:?}", r.p);
        assert!((r.p.y - 20.0).abs() < 1e-9, "y still on the grid");
        assert_eq!(r.align, vec![DVec2::new(3.27, 8.0)]);
    }

    #[test]
    fn horizontal_and_vertical() {
        let mut sk = Sketch::default();
        let a = sk.add_point(DVec2::new(0.3, 0.3));
        let r = line_end(&sk, a, DVec2::new(20.0, 0.6), WPP, None, &[], &HashSet::new());
        assert_eq!(r.dir, Some(Dir::Horizontal));
        assert!((r.p.y - 0.3).abs() < 1e-9);
        let r = line_end(&sk, a, DVec2::new(0.1, -15.0), WPP, None, &[], &HashSet::new());
        assert_eq!(r.dir, Some(Dir::Vertical));
        // 20° off: free
        let r = line_end(&sk, a, DVec2::new(20.0, 7.6), WPP, None, &[], &HashSet::new());
        assert_eq!(r.dir, None);
    }

    #[test]
    fn perpendicular_to_previous_segment() {
        // previous segment at 30°, ending at b; drawing on at ~120°
        let b = DVec2::new(30f64.to_radians().cos(), 30f64.to_radians().sin()) * 20.0;
        let (sk, _, pb, l) = sk_with_line(DVec2::ZERO, b);
        let perp = DVec2::new(-(30f64.to_radians().sin()), 30f64.to_radians().cos());
        let raw = b + perp * 15.0 + DVec2::new(0.3, 0.1);
        let r = line_end(&sk, pb, raw, WPP, Some(1.0), &[], &HashSet::new());
        assert_eq!(r.dir, Some(Dir::Perpendicular(l)));
        assert!((r.p - b).dot(b).abs() < 1e-9);
        // and the constraint it adds
        assert!(matches!(r.dir.unwrap().constraint(99), Constraint::Perpendicular { a, b: 99 } if a == l));
    }

    #[test]
    fn parallel_needs_a_woken_line() {
        let (mut sk, _, _, l) = sk_with_line(DVec2::new(0.0, 0.0), DVec2::new(20.0, 10.0));
        let s = sk.add_point(DVec2::new(0.0, 30.0));
        let raw = DVec2::new(20.0, 40.2);
        // not touching the anchor and not hovered: no inference
        let r = line_end(&sk, s, raw, WPP, None, &[], &HashSet::new());
        assert_eq!(r.dir, None);
        let r = line_end(&sk, s, raw, WPP, None, &[l], &HashSet::new());
        assert_eq!(r.dir, Some(Dir::Parallel(l)));
        assert!(((r.p - DVec2::new(0.0, 30.0)).perp_dot(DVec2::new(2.0, 1.0))).abs() < 1e-9);
    }

    #[test]
    fn horizontal_line_stops_under_a_point() {
        let mut sk = Sketch::default();
        let a = sk.add_point(DVec2::ZERO);
        sk.add_point(DVec2::new(12.34, 25.0));
        let r = line_end(&sk, a, DVec2::new(12.6, 0.2), WPP, Some(1.0), &[], &HashSet::new());
        assert_eq!(r.dir, Some(Dir::Horizontal));
        assert!((r.p - DVec2::new(12.34, 0.0)).length() < 1e-9, "{:?}", r.p);
        assert_eq!(r.align.len(), 1);
    }

    #[test]
    fn dropping_points() {
        let (mut sk, pa, pb, l) = sk_with_line(DVec2::ZERO, DVec2::new(10.0, 0.0));
        let c = sk.add_point(DVec2::new(10.0, 5.0));
        let d = sk.add_point(DVec2::new(10.2, 0.1));
        let m = sk.add_curve(Geom::Line { a: c, b: d }, false);
        // a line's own other end: refused
        assert!(!drop_point(&mut sk, pa, Drop::Point(pb)));
        assert!(!drop_point(&mut sk, pa, Drop::Curve(l)));
        // the end of another line: merged
        assert!(drop_point(&mut sk, d, Drop::Point(pb)));
        assert!(!sk.is_point(d));
        assert!(matches!(sk.curve(m).unwrap().geom, Geom::Line { b, .. } if b == pb));
        // onto a line and its midpoint, once each
        let e = sk.add_point(DVec2::new(5.0, 1.0));
        assert!(drop_point(&mut sk, e, Drop::Mid(l)));
        assert!(!drop_point(&mut sk, e, Drop::Mid(l)));
        assert!(drop_point(&mut sk, c, Drop::Origin));
        assert!(!drop_point(&mut sk, c, Drop::Origin));
    }

    #[test]
    fn short_lines_stay_free() {
        let mut sk = Sketch::default();
        let a = sk.add_point(DVec2::ZERO);
        // 2 px long: no direction yet
        let r = line_end(&sk, a, DVec2::new(0.2, 0.01), WPP, None, &[], &HashSet::new());
        assert_eq!(r.dir, None);
    }
}
