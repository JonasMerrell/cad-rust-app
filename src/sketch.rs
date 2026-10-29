//! 2D sketch data model and geometric constraint solver.
//!
//! Points carry two parameters each and circles carry their radius; arcs are
//! defined by centre, start and end points (counter-clockwise) with an implicit
//! equal-radius condition. Constraints and dimensions are residual functions,
//! solved with a damped minimum-norm Gauss-Newton step so unconstrained
//! geometry moves as little as possible (and the dragged point is held).

use std::collections::{HashMap, HashSet};

use glam::DVec2;
use serde::{Deserialize, Serialize};

use crate::expr::{self, Env, Kind};

pub type Id = u32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Point {
    pub id: Id,
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn v(&self) -> DVec2 {
        DVec2::new(self.x, self.y)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Geom {
    Line {
        a: Id,
        b: Id,
    },
    Circle {
        c: Id,
        r: f64,
    },
    /// Counter-clockwise from `a` to `b` around `c`.
    Arc {
        c: Id,
        a: Id,
        b: Id,
    },
    /// Full ellipse: centre, end of the major axis, minor radius.
    Ellipse {
        c: Id,
        a: Id,
        rmin: f64,
    },
    /// Part of an ellipse from parameter `t0` to `t1` (radians, counter-clockwise).
    EllipticalArc {
        c: Id,
        a: Id,
        rmin: f64,
        t0: f64,
        t1: f64,
    },
    /// Smooth curve through the points.
    Spline {
        pts: Vec<Id>,
        #[serde(default)]
        closed: bool,
    },
    /// Fixed outline (e.g. from text); not driven by the solver.
    Poly {
        pts: Vec<[f64; 2]>,
        closed: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Curve {
    pub id: Id,
    #[serde(flatten)]
    pub geom: Geom,
    #[serde(default)]
    pub construction: bool,
}

/// A driving dimension: the value is an expression in document units.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dim {
    pub expr: String,
    /// Label offset from the dimension's anchor, in sketch units.
    #[serde(default)]
    pub offset: [f64; 2],
    /// A reference (driven) dimension: it shows the measured size in parentheses and drives nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub driven: bool,
}

impl Dim {
    pub fn new(expr: impl Into<String>, offset: [f64; 2]) -> Dim {
        Dim { expr: expr.into(), offset, driven: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    PointOnLine {
        p: Id,
        l: Id,
    },
    PointOnCircle {
        p: Id,
        c: Id,
    },
    Horizontal {
        l: Id,
    },
    Vertical {
        l: Id,
    },
    HorizontalPts {
        a: Id,
        b: Id,
    },
    VerticalPts {
        a: Id,
        b: Id,
    },
    Parallel {
        a: Id,
        b: Id,
    },
    Perpendicular {
        a: Id,
        b: Id,
    },
    /// Line–circle or circle–circle tangency; `inner` for internal circle tangency.
    Tangent {
        a: Id,
        b: Id,
        #[serde(default)]
        inner: bool,
    },
    Equal {
        a: Id,
        b: Id,
    },
    Midpoint {
        p: Id,
        l: Id,
    },
    Fix {
        p: Id,
        x: f64,
        y: f64,
    },
    /// Points `a` and `b` mirror each other across line `l`.
    Symmetric {
        a: Id,
        b: Id,
        l: Id,
    },
    /// Point-to-point distance.
    Distance {
        a: Id,
        b: Id,
        dim: Dim,
    },
    /// Horizontal (`axis` 0) or vertical (1) distance between two points.
    AxisDistance {
        a: Id,
        b: Id,
        axis: u8,
        dim: Dim,
    },
    PointLineDistance {
        p: Id,
        l: Id,
        dim: Dim,
    },
    Length {
        l: Id,
        dim: Dim,
    },
    Radius {
        c: Id,
        dim: Dim,
    },
    Diameter {
        c: Id,
        dim: Dim,
    },
    /// Angle from line `a` to line `b` in degrees.
    Angle {
        a: Id,
        b: Id,
        dim: Dim,
    },
    /// Point `p` is as far from line `l` as point `q` is from line `m` (keeps an offset chain even).
    EqualDistance {
        p: Id,
        l: Id,
        q: Id,
        m: Id,
    },
}

impl Constraint {
    pub fn dim(&self) -> Option<&Dim> {
        use Constraint::*;
        match self {
            Distance { dim, .. }
            | AxisDistance { dim, .. }
            | PointLineDistance { dim, .. }
            | Length { dim, .. }
            | Radius { dim, .. }
            | Diameter { dim, .. }
            | Angle { dim, .. } => Some(dim),
            _ => None,
        }
    }

    pub fn dim_mut(&mut self) -> Option<&mut Dim> {
        use Constraint::*;
        match self {
            Distance { dim, .. }
            | AxisDistance { dim, .. }
            | PointLineDistance { dim, .. }
            | Length { dim, .. }
            | Radius { dim, .. }
            | Diameter { dim, .. }
            | Angle { dim, .. } => Some(dim),
            _ => None,
        }
    }

    pub fn is_angle(&self) -> bool {
        matches!(self, Constraint::Angle { .. })
    }

    /// Ids this constraint refers to (points or curves).
    pub fn refs(&self) -> Vec<Id> {
        use Constraint::*;
        match *self {
            PointOnLine { p, l } | Midpoint { p, l } | PointLineDistance { p, l, .. } => vec![p, l],
            PointOnCircle { p, c } => vec![p, c],
            Horizontal { l } | Vertical { l } | Length { l, .. } => vec![l],
            Radius { c, .. } | Diameter { c, .. } => vec![c],
            Fix { p, .. } => vec![p],
            Symmetric { a, b, l } => vec![a, b, l],
            EqualDistance { p, l, q, m } => vec![p, l, q, m],
            HorizontalPts { a, b }
            | VerticalPts { a, b }
            | Parallel { a, b }
            | Perpendicular { a, b }
            | Tangent { a, b, .. }
            | Equal { a, b }
            | Distance { a, b, .. }
            | AxisDistance { a, b, .. }
            | Angle { a, b, .. } => vec![a, b],
        }
    }

    pub fn icon(&self) -> &'static str {
        use Constraint::*;
        match self {
            PointOnLine { .. } | PointOnCircle { .. } => "coincident",
            Horizontal { .. } | HorizontalPts { .. } => "horizontal",
            Vertical { .. } | VerticalPts { .. } => "vertical",
            Parallel { .. } => "parallel",
            Perpendicular { .. } => "perpendicular",
            Tangent { .. } => "tangent",
            Equal { .. } | EqualDistance { .. } => "equal",
            Midpoint { .. } => "midpoint",
            Fix { .. } => "fix",
            Symmetric { .. } => "mirrorSketch",
            _ => "dimension",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    pub origin: [f64; 3],
    pub x: [f64; 3],
    pub y: [f64; 3],
    pub normal: [f64; 3],
}

impl Frame {
    pub fn to_world(&self, p: DVec2) -> glam::DVec3 {
        glam::DVec3::from(self.origin) + glam::DVec3::from(self.x) * p.x + glam::DVec3::from(self.y) * p.y
    }
    pub fn to_local(&self, w: glam::DVec3) -> DVec2 {
        let d = w - glam::DVec3::from(self.origin);
        DVec2::new(d.dot(self.x.into()), d.dot(self.y.into()))
    }
    pub fn normal(&self) -> glam::DVec3 {
        self.normal.into()
    }
    pub const TOP: Frame = Frame { origin: [0.0; 3], x: [1.0, 0.0, 0.0], y: [0.0, 1.0, 0.0], normal: [0.0, 0.0, 1.0] };
    pub const FRONT: Frame = Frame { origin: [0.0; 3], x: [1.0, 0.0, 0.0], y: [0.0, 0.0, 1.0], normal: [0.0, -1.0, 0.0] };
    pub const RIGHT: Frame = Frame { origin: [0.0; 3], x: [0.0, 1.0, 0.0], y: [0.0, 0.0, 1.0], normal: [1.0, 0.0, 0.0] };

    /// Intersect a world ray with this plane; returns local coordinates.
    pub fn ray_hit(&self, o: glam::DVec3, d: glam::DVec3) -> Option<DVec2> {
        let n = self.normal();
        let den = d.dot(n);
        if den.abs() < 1e-9 {
            return None;
        }
        let t = (glam::DVec3::from(self.origin) - o).dot(n) / den;
        Some(self.to_local(o + d * t))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sketch {
    pub points: Vec<Point>,
    pub curves: Vec<Curve>,
    pub constraints: Vec<Constraint>,
    pub next_id: Id,
    /// Text items: their outlines are curves regenerated from the text.
    #[serde(default)]
    pub texts: Vec<TextItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextItem {
    /// Baseline start point (a sketch point, so it can be dragged and constrained).
    pub anchor: Id,
    pub text: String,
    pub height: f64,
    /// Outline curves generated for it.
    pub curves: Vec<Id>,
}

#[derive(Clone, Debug, Default)]
pub struct SolveStatus {
    pub ok: bool,
    /// Remaining degrees of freedom (0 = fully defined).
    pub dof: usize,
    /// Points whose position is fully determined.
    pub fixed_points: HashSet<Id>,
    /// Curves whose parameters are all determined.
    pub fixed_curves: HashSet<Id>,
    pub error: Option<String>,
    /// Constraint indices whose dimension expression failed.
    pub bad_dims: Vec<usize>,
}

impl Sketch {
    /// Regenerate text outlines at their anchors (after edits or solving).
    pub fn refresh_texts(&mut self) {
        for i in 0..self.texts.len() {
            let t = self.texts[i].clone();
            let Some(at) = self.point(t.anchor).map(|p| p.v()) else { continue };
            let construction = t.curves.first().and_then(|c| self.curve(*c)).map(|c| c.construction).unwrap_or(false);
            let old: HashSet<Id> = t.curves.iter().copied().collect();
            self.curves.retain(|c| !old.contains(&c.id));
            let mut ids = vec![];
            for contour in crate::text::outlines(&t.text, at, t.height) {
                ids.push(self.add_curve(Geom::Poly { pts: contour.iter().map(|p| (*p).into()).collect(), closed: true }, construction));
            }
            self.texts[i].curves = ids;
        }
        // texts whose anchor was deleted go too
        let points: HashSet<Id> = self.points.iter().map(|p| p.id).collect();
        let gone: HashSet<Id> = self.texts.iter().filter(|t| !points.contains(&t.anchor)).flat_map(|t| t.curves.clone()).collect();
        self.curves.retain(|c| !gone.contains(&c.id));
        self.texts.retain(|t| points.contains(&t.anchor));
    }

    /// The text item an outline curve belongs to.
    pub fn text_of(&self, curve: Id) -> Option<usize> {
        self.texts.iter().position(|t| t.curves.contains(&curve))
    }

    pub fn new_id(&mut self) -> Id {
        self.next_id += 1;
        self.next_id
    }

    pub fn add_point(&mut self, p: DVec2) -> Id {
        let id = self.new_id();
        self.points.push(Point { id, x: p.x, y: p.y });
        id
    }

    pub fn add_curve(&mut self, geom: Geom, construction: bool) -> Id {
        let id = self.new_id();
        self.curves.push(Curve { id, geom, construction });
        id
    }

    pub fn point(&self, id: Id) -> Option<&Point> {
        self.points.iter().find(|p| p.id == id)
    }

    pub fn point_mut(&mut self, id: Id) -> Option<&mut Point> {
        self.points.iter_mut().find(|p| p.id == id)
    }

    pub fn pv(&self, id: Id) -> DVec2 {
        self.point(id).map(|p| p.v()).unwrap_or(DVec2::ZERO)
    }

    pub fn curve(&self, id: Id) -> Option<&Curve> {
        self.curves.iter().find(|c| c.id == id)
    }

    pub fn is_point(&self, id: Id) -> bool {
        self.point(id).is_some()
    }

    /// Points that are referenced by curves (endpoints and centres).
    pub fn curve_points(c: &Geom) -> Vec<Id> {
        match *c {
            Geom::Line { a, b } => vec![a, b],
            Geom::Circle { c, .. } => vec![c],
            Geom::Arc { c, a, b } => vec![c, a, b],
            Geom::Ellipse { c, a, .. } | Geom::EllipticalArc { c, a, .. } => vec![c, a],
            Geom::Spline { ref pts, .. } => pts.clone(),
            Geom::Poly { .. } => vec![],
        }
    }

    /// Replace every reference to point `from` by `to` and remove `from`.
    pub fn merge_point(&mut self, from: Id, to: Id) {
        if from == to {
            return;
        }
        let sub = |x: &mut Id| {
            if *x == from {
                *x = to
            }
        };
        for c in &mut self.curves {
            match &mut c.geom {
                Geom::Line { a, b } => {
                    sub(a);
                    sub(b)
                }
                Geom::Circle { c, .. } => sub(c),
                Geom::Arc { c, a, b } => {
                    sub(c);
                    sub(a);
                    sub(b)
                }
                Geom::Ellipse { c, a, .. } | Geom::EllipticalArc { c, a, .. } => {
                    sub(c);
                    sub(a)
                }
                Geom::Spline { pts, .. } => pts.iter_mut().for_each(sub),
                Geom::Poly { .. } => {}
            }
        }
        for k in &mut self.constraints {
            use Constraint::*;
            match k {
                PointOnLine { p, .. } | PointOnCircle { p, .. } | Midpoint { p, .. } | PointLineDistance { p, .. } | Fix { p, .. } => sub(p),
                HorizontalPts { a, b } | VerticalPts { a, b } | Distance { a, b, .. } | AxisDistance { a, b, .. } | Symmetric { a, b, .. } => {
                    sub(a);
                    sub(b)
                }
                EqualDistance { p, q, .. } => {
                    sub(p);
                    sub(q)
                }
                _ => {}
            }
        }
        self.points.retain(|p| p.id != from);
        // Drop constraints that became degenerate (e.g. distance between a point and itself).
        self.constraints.retain(|k| match k {
            Constraint::HorizontalPts { a, b } | Constraint::VerticalPts { a, b } | Constraint::Distance { a, b, .. } | Constraint::AxisDistance { a, b, .. } => a != b,
            _ => true,
        });
        self.dedupe_points_on_curves();
    }

    fn dedupe_points_on_curves(&mut self) {
        // A point-on-curve constraint whose point is already an endpoint of the curve is redundant.
        let curves = self.curves.clone();
        self.constraints.retain(|k| match *k {
            Constraint::PointOnLine { p, l } | Constraint::PointOnCircle { p, c: l } => {
                curves.iter().find(|c| c.id == l).map(|c| !Self::curve_points(&c.geom).contains(&p)).unwrap_or(false)
            }
            _ => true,
        });
    }

    /// Delete the given points/curves/constraints (by id / index) and anything depending on them.
    pub fn delete(&mut self, ids: &HashSet<Id>, constraint_idx: &HashSet<usize>) {
        let mut dead: HashSet<Id> = ids.clone();
        for c in &self.curves {
            if Self::curve_points(&c.geom).iter().any(|p| dead.contains(p)) {
                dead.insert(c.id);
            }
        }
        // endpoints/centres of deleted curves go too, unless another curve still uses them
        let freed: HashSet<Id> = self.curves.iter().filter(|c| dead.contains(&c.id)).flat_map(|c| Self::curve_points(&c.geom)).collect();
        self.curves.retain(|c| !dead.contains(&c.id));
        let used: HashSet<Id> = self.curves.iter().flat_map(|c| Self::curve_points(&c.geom)).collect();
        self.points.retain(|p| !dead.contains(&p.id) && (used.contains(&p.id) || !freed.contains(&p.id)));
        let alive: HashSet<Id> = self.points.iter().map(|p| p.id).chain(self.curves.iter().map(|c| c.id)).collect();
        let mut i = 0;
        self.constraints.retain(|k| {
            let keep = !constraint_idx.contains(&i) && k.refs().iter().all(|r| alive.contains(r));
            i += 1;
            keep
        });
    }

    // ---------------------------------------------------------------- solver

    /// Solve all constraints, keeping the points in `hold` (e.g. the dragged one) as still as possible.
    pub fn solve(&mut self, env: &Env, hold: &[Id]) -> SolveStatus {
        self.solve_without(env, hold, &[])
    }

    /// Solve with the constraints at the indices in `skip` left out.
    fn solve_without(&mut self, env: &Env, hold: &[Id], skip: &[usize]) -> SolveStatus {
        let sys = System::build(self, env, skip);
        let mut st = SolveStatus { bad_dims: sys.bad_dims.clone(), ..Default::default() };
        let mut x = sys.x0.clone();
        let mut w = vec![1.0; x.len()];
        for h in hold {
            if let Some(&i) = sys.pidx.get(h) {
                w[i] = 1e4;
                w[i + 1] = 1e4;
            }
        }
        let mut ok = false;
        for _ in 0..60 {
            let r = sys.residuals(&x);
            let norm = norm2(&r);
            if norm < 1e-10 {
                ok = true;
                break;
            }
            let j = sys.jacobian(&x);
            let dx = min_norm_step(&j, &r, &w, x.len());
            // backtracking
            let mut t = 1.0;
            let mut improved = false;
            for _ in 0..12 {
                let xn: Vec<f64> = x.iter().zip(&dx).map(|(a, d)| a + t * d).collect();
                if norm2(&sys.residuals(&xn)) < norm {
                    x = xn;
                    improved = true;
                    break;
                }
                t *= 0.5;
            }
            if !improved {
                break;
            }
        }
        if !ok {
            ok = norm2(&sys.residuals(&x)) < 1e-8;
        }
        // DOF analysis at the solution
        let j = sys.jacobian(&x);
        let (rank, null) = null_space(&j, x.len());
        st.dof = x.len() - rank;
        let determined = |i: usize| null.iter().all(|v| v[i].abs() < 1e-7);
        for (&id, &i) in &sys.pidx {
            if determined(i) && determined(i + 1) {
                st.fixed_points.insert(id);
            }
        }
        for c in &self.curves {
            let pts_ok = Self::curve_points(&c.geom).iter().all(|p| st.fixed_points.contains(p));
            let r_ok = sys.ridx.get(&c.id).map(|&i| determined(i)).unwrap_or(true);
            if pts_ok && r_ok {
                st.fixed_curves.insert(c.id);
            }
        }
        if ok {
            sys.write_back(self, &x);
        } else {
            st.error = Some("Sketch constraints conflict".into());
        }
        st.ok = ok;
        st
    }
}

impl Sketch {
    /// The constraints that, each taken out on its own, let the sketch solve again: what
    /// stops a sketch that doesn't solve. Constraint `keep` (the one just changed) stays in.
    pub fn conflicts(&self, env: &Env, keep: usize) -> Vec<usize> {
        if self.constraints.len() > 400 {
            return vec![];
        }
        (0..self.constraints.len())
            .filter(|&i| i != keep && !self.constraints[i].dim().map(|d| d.driven).unwrap_or(false))
            .filter(|&i| self.clone().solve_without(env, &[], &[i]).ok)
            .collect()
    }

    /// The size a dimension measures on the current geometry: mm, or degrees for angles.
    pub fn measure(&self, k: &Constraint) -> Option<f64> {
        use Constraint::*;
        let line = |l: Id| match self.curve(l).map(|c| &c.geom) {
            Some(Geom::Line { a, b }) => Some((self.pv(*a), self.pv(*b))),
            _ => None,
        };
        let radius = |c: Id| match self.curve(c).map(|c| &c.geom) {
            Some(Geom::Circle { r, .. }) => Some(*r),
            Some(Geom::Arc { c, a, .. }) => Some((self.pv(*a) - self.pv(*c)).length()),
            _ => None,
        };
        Some(match *k {
            Distance { a, b, .. } => (self.pv(b) - self.pv(a)).length(),
            AxisDistance { a, b, axis, .. } => (self.pv(b) - self.pv(a))[axis as usize].abs(),
            PointLineDistance { p, l, .. } => {
                let (a, b) = line(l)?;
                ((b - a).perp_dot(self.pv(p) - a) / (b - a).length().max(1e-12)).abs()
            }
            Length { l, .. } => {
                let (a, b) = line(l)?;
                (b - a).length()
            }
            Radius { c, .. } => radius(c)?,
            Diameter { c, .. } => 2.0 * radius(c)?,
            Angle { a, b, .. } => {
                let ((p, q), (r, t)) = (line(a)?, line(b)?);
                // the residual allows theta and theta + 180°: report the one in [0, 180)
                (q - p).angle_to(t - r).to_degrees().rem_euclid(180.0)
            }
            _ => return None,
        })
    }

    /// The points connected to `id` (a point or curve) through curves and constraints.
    pub fn group_of(&self, id: Id) -> HashSet<Id> {
        let mut links: HashMap<Id, Vec<Id>> = HashMap::new();
        let mut link = |a: Id, b: Id| {
            links.entry(a).or_default().push(b);
            links.entry(b).or_default().push(a);
        };
        for c in &self.curves {
            for p in Self::curve_points(&c.geom) {
                link(c.id, p);
            }
        }
        for k in &self.constraints {
            let r = k.refs();
            for w in r.windows(2) {
                link(w[0], w[1]);
            }
        }
        let mut seen: HashSet<Id> = [id].into();
        let mut todo = vec![id];
        while let Some(x) = todo.pop() {
            for y in links.get(&x).into_iter().flatten() {
                if seen.insert(*y) {
                    todo.push(*y);
                }
            }
        }
        seen.retain(|x| self.is_point(*x));
        seen
    }
}

fn norm2(r: &[f64]) -> f64 {
    r.iter().map(|v| v * v).sum::<f64>()
}

/// Residual of one constraint as a closure over parameter indices.
enum Res {
    Fix(usize, f64),
    /// cross((b-a),(p-a))/|b-a|
    PointLine {
        p: usize,
        a: usize,
        b: usize,
    },
    /// |p-c| - r  where r is param `r` or |q-c|
    PointCircle {
        p: usize,
        c: usize,
        r: RIdx,
    },
    Horiz(usize, usize),
    Vert(usize, usize),
    Parallel(usize, usize, usize, usize),
    Perp(usize, usize, usize, usize),
    Angle(usize, usize, usize, usize, f64),
    LineCircleTangent {
        a: usize,
        b: usize,
        c: usize,
        r: RIdx,
    },
    CircleCircle {
        c1: usize,
        r1: RIdx,
        c2: usize,
        r2: RIdx,
        inner: bool,
    },
    EqualLen(usize, usize, usize, usize),
    EqualR(RIdx, RIdx),
    MidX {
        p: usize,
        a: usize,
        b: usize,
    },
    Dist(usize, usize, f64),
    AxisDist(usize, usize, usize, f64),
    PointLineDist {
        p: usize,
        a: usize,
        b: usize,
        d: f64,
    },
    RadiusIs(RIdx, f64),
    /// The radius to `p` (an end shared with line a-b) is perpendicular to the line.
    PerpAt {
        p: usize,
        c: usize,
        a: usize,
        b: usize,
    },
    /// `p` (an end two arcs share) lies on the line through both centres.
    CollinearAt {
        p: usize,
        c1: usize,
        c2: usize,
    },
    /// |dist(p, line a-b)| - |dist(q, line c-d)|
    EqualDist {
        p: usize,
        a: usize,
        b: usize,
        q: usize,
        c: usize,
        d: usize,
    },
    /// midpoint of p,q on line a-b and p-q perpendicular to it
    Symmetric {
        p: usize,
        q: usize,
        a: usize,
        b: usize,
    },
}

#[derive(Clone, Copy)]
enum RIdx {
    Param(usize),
    /// radius as distance between centre (first) and a point (second)
    Pts(usize, usize),
}

struct System {
    x0: Vec<f64>,
    pidx: HashMap<Id, usize>,
    ridx: HashMap<Id, usize>,
    res: Vec<Res>,
    bad_dims: Vec<usize>,
}

fn p2(x: &[f64], i: usize) -> DVec2 {
    DVec2::new(x[i], x[i + 1])
}

fn rad(x: &[f64], r: RIdx) -> f64 {
    match r {
        RIdx::Param(i) => x[i],
        RIdx::Pts(c, p) => (p2(x, p) - p2(x, c)).length(),
    }
}

impl Res {
    fn eval(&self, x: &[f64], out: &mut Vec<f64>) {
        match *self {
            Res::Fix(i, v) => out.push(x[i] - v),
            Res::PointLine { p, a, b } => {
                let d = p2(x, b) - p2(x, a);
                let l = d.length().max(1e-9);
                out.push(d.perp_dot(p2(x, p) - p2(x, a)) / l)
            }
            Res::PointCircle { p, c, r } => out.push((p2(x, p) - p2(x, c)).length() - rad(x, r)),
            Res::Horiz(a, b) => out.push(x[a + 1] - x[b + 1]),
            Res::Vert(a, b) => out.push(x[a] - x[b]),
            Res::Parallel(a, b, c, d) => {
                let u = (p2(x, b) - p2(x, a)).normalize_or_zero();
                let v = (p2(x, d) - p2(x, c)).normalize_or_zero();
                out.push(u.perp_dot(v) * 10.0)
            }
            Res::Perp(a, b, c, d) => {
                let u = (p2(x, b) - p2(x, a)).normalize_or_zero();
                let v = (p2(x, d) - p2(x, c)).normalize_or_zero();
                out.push(u.dot(v) * 10.0)
            }
            Res::Angle(a, b, c, d, deg) => {
                let u = (p2(x, b) - p2(x, a)).normalize_or_zero();
                let v = (p2(x, d) - p2(x, c)).normalize_or_zero();
                let t = deg.to_radians();
                // sin(theta - t) where theta is the angle from u to v
                out.push((u.perp_dot(v) * t.cos() - u.dot(v) * t.sin()) * 10.0)
            }
            Res::LineCircleTangent { a, b, c, r } => {
                let d = p2(x, b) - p2(x, a);
                let l = d.length().max(1e-9);
                out.push((d.perp_dot(p2(x, c) - p2(x, a)) / l).abs() - rad(x, r))
            }
            Res::CircleCircle { c1, r1, c2, r2, inner } => {
                let dist = (p2(x, c2) - p2(x, c1)).length();
                let want = if inner { (rad(x, r1) - rad(x, r2)).abs() } else { rad(x, r1) + rad(x, r2) };
                out.push(dist - want)
            }
            Res::EqualLen(a, b, c, d) => out.push((p2(x, b) - p2(x, a)).length() - (p2(x, d) - p2(x, c)).length()),
            Res::EqualR(a, b) => out.push(rad(x, a) - rad(x, b)),
            Res::MidX { p, a, b } => {
                let m = (p2(x, a) + p2(x, b)) * 0.5;
                out.push(x[p] - m.x);
                out.push(x[p + 1] - m.y);
            }
            Res::Dist(a, b, d) => out.push((p2(x, b) - p2(x, a)).length() - d),
            Res::AxisDist(a, b, axis, d) => out.push((x[b + axis] - x[a + axis]).abs() - d),
            Res::PointLineDist { p, a, b, d } => {
                let dd = p2(x, b) - p2(x, a);
                let l = dd.length().max(1e-9);
                out.push((dd.perp_dot(p2(x, p) - p2(x, a)) / l).abs() - d)
            }
            Res::RadiusIs(r, v) => out.push(rad(x, r) - v),
            Res::PerpAt { p, c, a, b } => {
                let u = (p2(x, p) - p2(x, c)).normalize_or_zero();
                let v = (p2(x, b) - p2(x, a)).normalize_or_zero();
                out.push(u.dot(v) * 10.0)
            }
            Res::CollinearAt { p, c1, c2 } => {
                let u = (p2(x, p) - p2(x, c1)).normalize_or_zero();
                let v = (p2(x, p) - p2(x, c2)).normalize_or_zero();
                out.push(u.perp_dot(v) * 10.0)
            }
            Res::EqualDist { p, a, b, q, c, d } => {
                let dist = |p: usize, a: usize, b: usize| {
                    let dd = p2(x, b) - p2(x, a);
                    (dd.perp_dot(p2(x, p) - p2(x, a)) / dd.length().max(1e-9)).abs()
                };
                out.push(dist(p, a, b) - dist(q, c, d))
            }
            Res::Symmetric { p, q, a, b } => {
                let d = p2(x, b) - p2(x, a);
                let l = d.length().max(1e-9);
                let m = (p2(x, p) + p2(x, q)) * 0.5;
                out.push(d.perp_dot(m - p2(x, a)) / l);
                out.push(d.dot(p2(x, q) - p2(x, p)) / l);
            }
        }
    }

    fn deps(&self) -> Vec<usize> {
        let pt = |i: usize| [i, i + 1];
        let r = |r: RIdx| match r {
            RIdx::Param(i) => vec![i],
            RIdx::Pts(c, p) => vec![c, c + 1, p, p + 1],
        };
        let mut v: Vec<usize> = match *self {
            Res::Fix(i, _) => vec![i],
            Res::PointLine { p, a, b } | Res::MidX { p, a, b } | Res::PointLineDist { p, a, b, .. } => [pt(p), pt(a), pt(b)].concat(),
            Res::PointCircle { p, c, r: rr } => [pt(p).to_vec(), pt(c).to_vec(), r(rr)].concat(),
            Res::Horiz(a, b) | Res::Vert(a, b) | Res::Dist(a, b, _) | Res::AxisDist(a, b, _, _) => [pt(a), pt(b)].concat(),
            Res::Parallel(a, b, c, d) | Res::Perp(a, b, c, d) | Res::Angle(a, b, c, d, _) | Res::EqualLen(a, b, c, d) => [pt(a), pt(b), pt(c), pt(d)].concat(),
            Res::LineCircleTangent { a, b, c, r: rr } => [pt(a).to_vec(), pt(b).to_vec(), pt(c).to_vec(), r(rr)].concat(),
            Res::CircleCircle { c1, r1, c2, r2, .. } => [pt(c1).to_vec(), r(r1), pt(c2).to_vec(), r(r2)].concat(),
            Res::EqualR(a, b) => [r(a), r(b)].concat(),
            Res::RadiusIs(rr, _) => r(rr),
            Res::Symmetric { p, q, a, b } => [pt(p), pt(q), pt(a), pt(b)].concat(),
            Res::EqualDist { p, a, b, q, c, d } => [pt(p), pt(a), pt(b), pt(q), pt(c), pt(d)].concat(),
            Res::PerpAt { p, c, a, b } => [pt(p), pt(c), pt(a), pt(b)].concat(),
            Res::CollinearAt { p, c1, c2 } => [pt(p), pt(c1), pt(c2)].concat(),
        };
        v.sort_unstable();
        v.dedup();
        v
    }
}

impl System {
    fn build(s: &Sketch, env: &Env, skip: &[usize]) -> System {
        let mut x0 = vec![];
        let mut pidx = HashMap::new();
        let mut ridx = HashMap::new();
        for p in &s.points {
            pidx.insert(p.id, x0.len());
            x0.push(p.x);
            x0.push(p.y);
        }
        for c in &s.curves {
            if let Geom::Circle { r, .. } | Geom::Ellipse { rmin: r, .. } | Geom::EllipticalArc { rmin: r, .. } = c.geom {
                ridx.insert(c.id, x0.len());
                x0.push(r);
            }
        }
        let mut res = vec![];
        let curve = |id: Id| s.curve(id).map(|c| c.geom.clone());
        let line = |id: Id| match curve(id) {
            Some(Geom::Line { a, b }) => Some((pidx[&a], pidx[&b])),
            _ => None,
        };
        let circ = |id: Id| match curve(id) {
            Some(Geom::Circle { c, .. }) => Some((pidx[&c], RIdx::Param(ridx[&id]))),
            Some(Geom::Arc { c, a, .. }) => Some((pidx[&c], RIdx::Pts(pidx[&c], pidx[&a]))),
            _ => None,
        };
        // arcs: implicit equal radius of both ends
        for c in &s.curves {
            if let Geom::Arc { c, a, b } = c.geom {
                res.push(Res::PointCircle { p: pidx[&b], c: pidx[&c], r: RIdx::Pts(pidx[&c], pidx[&a]) });
            }
        }
        let mut bad_dims = vec![];
        for (ki, k) in s.constraints.iter().enumerate() {
            // reference dimensions only measure
            if skip.contains(&ki) || k.dim().map(|d| d.driven).unwrap_or(false) {
                continue;
            }
            let val = |d: &Dim, kind: Kind| -> Option<f64> { expr::eval(&d.expr, kind, env).ok() };
            let pi = |id: Id| pidx.get(&id).copied();
            use Constraint::*;
            let r: Option<Vec<Res>> = (|| {
                Some(match k {
                    PointOnLine { p, l } => {
                        let (a, b) = line(*l)?;
                        vec![Res::PointLine { p: pi(*p)?, a, b }]
                    }
                    PointOnCircle { p, c } => {
                        let (cc, r) = circ(*c)?;
                        vec![Res::PointCircle { p: pi(*p)?, c: cc, r }]
                    }
                    Horizontal { l } => {
                        let (a, b) = line(*l)?;
                        vec![Res::Horiz(a, b)]
                    }
                    Vertical { l } => {
                        let (a, b) = line(*l)?;
                        vec![Res::Vert(a, b)]
                    }
                    HorizontalPts { a, b } => vec![Res::Horiz(pi(*a)?, pi(*b)?)],
                    VerticalPts { a, b } => vec![Res::Vert(pi(*a)?, pi(*b)?)],
                    Parallel { a, b } => {
                        let (p, q) = line(*a)?;
                        let (r, t) = line(*b)?;
                        vec![Res::Parallel(p, q, r, t)]
                    }
                    Perpendicular { a, b } => {
                        let (p, q) = line(*a)?;
                        let (r, t) = line(*b)?;
                        vec![Res::Perp(p, q, r, t)]
                    }
                    Tangent { a, b, inner } => {
                        // Curves that already share an end point are tangent where they meet. Measuring that
                        // directly matters: the distance form has a zero gradient there (it is at a minimum),
                        // which hid the constraint from the rank count and over-reported the DOF.
                        let ends = |id: Id| match curve(id) {
                            Some(Geom::Line { a, b }) | Some(Geom::Arc { a, b, .. }) => vec![a, b],
                            _ => vec![],
                        };
                        let centre = |id: Id| match curve(id) {
                            Some(Geom::Arc { c, .. }) => Some(c),
                            _ => None,
                        };
                        let shared = ends(*a).into_iter().find(|p| ends(*b).contains(p));
                        let (ca, cb) = (centre(*a), centre(*b));
                        if let Some(p) = shared.filter(|_| ca.is_some() || cb.is_some()) {
                            match (line(*a).or(line(*b)), ca, cb) {
                                (Some((la, lb)), Some(c), None) | (Some((la, lb)), None, Some(c)) => vec![Res::PerpAt { p: pi(p)?, c: pi(c)?, a: la, b: lb }],
                                (None, Some(c1), Some(c2)) => vec![Res::CollinearAt { p: pi(p)?, c1: pi(c1)?, c2: pi(c2)? }],
                                _ => return None,
                            }
                        } else if let Some((la, lb)) = line(*a) {
                            let (c, r) = circ(*b)?;
                            vec![Res::LineCircleTangent { a: la, b: lb, c, r }]
                        } else if let Some((la, lb)) = line(*b) {
                            let (c, r) = circ(*a)?;
                            vec![Res::LineCircleTangent { a: la, b: lb, c, r }]
                        } else {
                            let (c1, r1) = circ(*a)?;
                            let (c2, r2) = circ(*b)?;
                            vec![Res::CircleCircle { c1, r1, c2, r2, inner: *inner }]
                        }
                    }
                    Equal { a, b } => {
                        if let (Some((p, q)), Some((r, t))) = (line(*a), line(*b)) {
                            vec![Res::EqualLen(p, q, r, t)]
                        } else {
                            vec![Res::EqualR(circ(*a)?.1, circ(*b)?.1)]
                        }
                    }
                    Midpoint { p, l } => {
                        let (a, b) = line(*l)?;
                        vec![Res::MidX { p: pi(*p)?, a, b }]
                    }
                    Fix { p, x, y } => {
                        let i = pi(*p)?;
                        vec![Res::Fix(i, *x), Res::Fix(i + 1, *y)]
                    }
                    Symmetric { a, b, l } => {
                        let (la, lb) = line(*l)?;
                        vec![Res::Symmetric { p: pi(*a)?, q: pi(*b)?, a: la, b: lb }]
                    }
                    Distance { a, b, dim } => vec![Res::Dist(pi(*a)?, pi(*b)?, val(dim, Kind::Length)?)],
                    AxisDistance { a, b, axis, dim } => vec![Res::AxisDist(pi(*a)?, pi(*b)?, *axis as usize, val(dim, Kind::Length)?)],
                    PointLineDistance { p, l, dim } => {
                        let (a, b) = line(*l)?;
                        vec![Res::PointLineDist { p: pi(*p)?, a, b, d: val(dim, Kind::Length)? }]
                    }
                    Length { l, dim } => {
                        let (a, b) = line(*l)?;
                        vec![Res::Dist(a, b, val(dim, Kind::Length)?)]
                    }
                    Radius { c, dim } => vec![Res::RadiusIs(circ(*c)?.1, val(dim, Kind::Length)?)],
                    Diameter { c, dim } => vec![Res::RadiusIs(circ(*c)?.1, val(dim, Kind::Length)? / 2.0)],
                    Angle { a, b, dim } => {
                        let (p, q) = line(*a)?;
                        let (r, t) = line(*b)?;
                        vec![Res::Angle(p, q, r, t, val(dim, Kind::Angle)?)]
                    }
                    EqualDistance { p, l, q, m } => {
                        let (a, b) = line(*l)?;
                        let (c, d) = line(*m)?;
                        vec![Res::EqualDist { p: pi(*p)?, a, b, q: pi(*q)?, c, d }]
                    }
                })
            })();
            match r {
                Some(r) => res.extend(r),
                None => {
                    if k.dim().is_some() {
                        bad_dims.push(ki)
                    }
                }
            }
        }
        System { x0, pidx, ridx, res, bad_dims }
    }

    fn residuals(&self, x: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.res.len());
        for r in &self.res {
            r.eval(x, &mut out);
        }
        out
    }

    /// Dense Jacobian (rows = residuals) by central differences per dependency.
    fn jacobian(&self, x: &[f64]) -> Vec<Vec<f64>> {
        let n = x.len();
        let mut rows = vec![];
        let mut xp = x.to_vec();
        let mut buf = vec![];
        for r in &self.res {
            buf.clear();
            r.eval(x, &mut buf);
            let m = buf.len();
            let mut local = vec![vec![0.0; n]; m];
            for i in r.deps() {
                let h = 1e-7 * (1.0 + x[i].abs());
                xp[i] = x[i] + h;
                let mut fp = vec![];
                r.eval(&xp, &mut fp);
                xp[i] = x[i] - h;
                let mut fm = vec![];
                r.eval(&xp, &mut fm);
                xp[i] = x[i];
                for k in 0..m {
                    local[k][i] = (fp[k] - fm[k]) / (2.0 * h);
                }
            }
            rows.extend(local);
        }
        rows
    }

    fn write_back(&self, s: &mut Sketch, x: &[f64]) {
        for p in &mut s.points {
            let i = self.pidx[&p.id];
            p.x = x[i];
            p.y = x[i + 1];
        }
        for c in &mut s.curves {
            if let Geom::Circle { r, .. } | Geom::Ellipse { rmin: r, .. } | Geom::EllipticalArc { rmin: r, .. } = &mut c.geom {
                *r = x[self.ridx[&c.id]].abs();
            }
        }
    }
}

/// dx = -W⁻¹Jᵀ (J W⁻¹ Jᵀ + μI)⁻¹ r
pub fn min_norm_step(j: &[Vec<f64>], r: &[f64], w: &[f64], n: usize) -> Vec<f64> {
    let m = j.len();
    if m == 0 {
        return vec![0.0; n];
    }
    let mut a = vec![vec![0.0; m]; m];
    for p in 0..m {
        for q in p..m {
            let mut s = 0.0;
            for k in 0..n {
                if j[p][k] != 0.0 && j[q][k] != 0.0 {
                    s += j[p][k] * j[q][k] / w[k];
                }
            }
            a[p][q] = s;
            a[q][p] = s;
        }
    }
    let tr: f64 = (0..m).map(|i| a[i][i]).sum::<f64>() / m as f64;
    for i in 0..m {
        a[i][i] += 1e-10 * (1.0 + tr);
    }
    let y = solve_dense(a, r.to_vec());
    let mut dx = vec![0.0; n];
    for p in 0..m {
        for k in 0..n {
            dx[k] -= j[p][k] * y[p] / w[k];
        }
    }
    dx
}

pub fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let n = b.len();
    for c in 0..n {
        let piv = (c..n).max_by(|&i, &k| a[i][c].abs().total_cmp(&a[k][c].abs())).unwrap();
        a.swap(c, piv);
        b.swap(c, piv);
        let d = a[c][c];
        if d.abs() < 1e-14 {
            continue;
        }
        for r in c + 1..n {
            let f = a[r][c] / d;
            if f != 0.0 {
                for k in c..n {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    let mut x = vec![0.0; n];
    for c in (0..n).rev() {
        let mut s = b[c];
        for k in c + 1..n {
            s -= a[c][k] * x[k];
        }
        x[c] = if a[c][c].abs() < 1e-14 { 0.0 } else { s / a[c][c] };
    }
    x
}

/// Rank of J and an orthonormal-ish basis of its null space (via RREF).
fn null_space(j: &[Vec<f64>], n: usize) -> (usize, Vec<Vec<f64>>) {
    let mut a: Vec<Vec<f64>> = j.to_vec();
    // normalise rows for a scale-independent tolerance
    for r in &mut a {
        let l = r.iter().map(|v| v * v).sum::<f64>().sqrt();
        if l > 0.0 {
            r.iter_mut().for_each(|v| *v /= l);
        }
    }
    let m = a.len();
    let mut pivots = vec![];
    let mut row = 0;
    for c in 0..n {
        if row >= m {
            break;
        }
        let piv = (row..m).max_by(|&i, &k| a[i][c].abs().total_cmp(&a[k][c].abs())).unwrap();
        if a[piv][c].abs() < 1e-6 {
            continue;
        }
        a.swap(row, piv);
        let d = a[row][c];
        a[row].iter_mut().for_each(|v| *v /= d);
        for r in 0..m {
            if r != row {
                let f = a[r][c];
                if f != 0.0 {
                    for k in 0..n {
                        a[r][k] -= f * a[row][k];
                    }
                }
            }
        }
        pivots.push(c);
        row += 1;
    }
    let rank = pivots.len();
    let piv_set: HashSet<usize> = pivots.iter().copied().collect();
    let mut basis = vec![];
    for f in (0..n).filter(|c| !piv_set.contains(c)) {
        let mut v = vec![0.0; n];
        v[f] = 1.0;
        for (r, &pc) in pivots.iter().enumerate() {
            v[pc] = -a[r][f];
        }
        basis.push(v);
    }
    (rank, basis)
}

// ------------------------------------------------------------- geometry helpers

pub fn arc_angles(c: DVec2, a: DVec2, b: DVec2) -> (f64, f64) {
    let a0 = (a - c).to_angle();
    let mut a1 = (b - c).to_angle();
    while a1 <= a0 {
        a1 += std::f64::consts::TAU;
    }
    (a0, a1)
}

impl Sketch {
    /// Polyline approximation of a curve in sketch coordinates.
    pub fn tessellate(&self, c: &Curve, seg_len: f64) -> Vec<DVec2> {
        match c.geom {
            Geom::Line { a, b } => vec![self.pv(a), self.pv(b)],
            Geom::Circle { c: cc, r } => {
                let ctr = self.pv(cc);
                let n = ((std::f64::consts::TAU * r / seg_len).ceil() as usize).clamp(24, 256);
                (0..=n).map(|i| ctr + DVec2::from_angle(i as f64 / n as f64 * std::f64::consts::TAU) * r).collect()
            }
            Geom::Arc { c: cc, a, b } => {
                let ctr = self.pv(cc);
                let r = (self.pv(a) - ctr).length();
                let (a0, a1) = arc_angles(ctr, self.pv(a), self.pv(b));
                let n = (((a1 - a0) * r / seg_len).ceil() as usize).clamp(8, 256);
                (0..=n).map(|i| ctr + DVec2::from_angle(a0 + (a1 - a0) * i as f64 / n as f64) * r).collect()
            }
            Geom::Ellipse { c: cc, a, rmin } => {
                let ctr = self.pv(cc);
                let u = self.pv(a) - ctr;
                let v = u.perp().normalize_or_zero() * rmin;
                let n = ((std::f64::consts::TAU * u.length().max(rmin) / seg_len).ceil() as usize).clamp(32, 360);
                (0..=n)
                    .map(|i| {
                        let t = i as f64 / n as f64 * std::f64::consts::TAU;
                        ctr + u * t.cos() + v * t.sin()
                    })
                    .collect()
            }
            Geom::EllipticalArc { c: cc, a, rmin, t0, t1 } => {
                let ctr = self.pv(cc);
                let u = self.pv(a) - ctr;
                let v = u.perp().normalize_or_zero() * rmin;
                let n = (((t1 - t0).abs() * u.length().max(rmin) / seg_len).ceil() as usize).clamp(8, 360);
                (0..=n)
                    .map(|i| {
                        let t = t0 + (t1 - t0) * i as f64 / n as f64;
                        ctr + u * t.cos() + v * t.sin()
                    })
                    .collect()
            }
            Geom::Spline { ref pts, closed } => {
                let p: Vec<DVec2> = pts.iter().map(|id| self.pv(*id)).collect();
                catmull_rom(&p, closed, 16)
            }
            Geom::Poly { ref pts, closed } => {
                let mut v: Vec<DVec2> = pts.iter().map(|p| DVec2::from(*p)).collect();
                if closed && !v.is_empty() {
                    v.push(v[0]);
                }
                v
            }
        }
    }

    /// Distance from `p` to the curve.
    pub fn curve_distance(&self, c: &Curve, p: DVec2) -> f64 {
        match c.geom {
            Geom::Line { a, b } => seg_dist(p, self.pv(a), self.pv(b)),
            Geom::Circle { c: cc, r } => ((p - self.pv(cc)).length() - r).abs(),
            Geom::Arc { c: cc, a, b } => {
                let ctr = self.pv(cc);
                let r = (self.pv(a) - ctr).length();
                let (a0, a1) = arc_angles(ctr, self.pv(a), self.pv(b));
                let mut t = (p - ctr).to_angle();
                while t < a0 {
                    t += std::f64::consts::TAU;
                }
                if t <= a1 { ((p - ctr).length() - r).abs() } else { (p - self.pv(a)).length().min((p - self.pv(b)).length()) }
            }
            _ => {
                let pts = self.tessellate(c, 0.5);
                pts.windows(2).map(|w| seg_dist(p, w[0], w[1])).fold(f64::INFINITY, f64::min)
            }
        }
    }

    pub fn bounds(&self) -> Option<(DVec2, DVec2)> {
        let mut lo = DVec2::splat(f64::INFINITY);
        let mut hi = DVec2::splat(f64::NEG_INFINITY);
        for c in &self.curves {
            for p in self.tessellate(c, 5.0) {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        for p in &self.points {
            lo = lo.min(p.v());
            hi = hi.max(p.v());
        }
        lo.x.is_finite().then_some((lo, hi))
    }
}

/// Centripetal Catmull-Rom curve through `p` with `per` samples per span.
pub fn catmull_rom(p: &[DVec2], closed: bool, per: usize) -> Vec<DVec2> {
    let n = p.len();
    if n < 2 {
        return p.to_vec();
    }
    let get = |i: isize| -> DVec2 {
        if closed {
            p[i.rem_euclid(n as isize) as usize]
        } else if i < 0 {
            p[0] * 2.0 - p[1]
        } else if i as usize >= n {
            p[n - 1] * 2.0 - p[n - 2]
        } else {
            p[i as usize]
        }
    };
    let spans = if closed { n } else { n - 1 };
    let mut out = vec![];
    for s in 0..spans as isize {
        let (p0, p1, p2, p3) = (get(s - 1), get(s), get(s + 1), get(s + 2));
        let kt = |a: DVec2, b: DVec2| (b - a).length().sqrt().max(1e-9);
        let t1 = kt(p0, p1);
        let t2 = t1 + kt(p1, p2);
        let t3 = t2 + kt(p2, p3);
        for k in 0..per {
            let t = t1 + (t2 - t1) * k as f64 / per as f64;
            let a1 = p0 * ((t1 - t) / t1) + p1 * (t / t1);
            let a2 = p1 * ((t2 - t) / (t2 - t1)) + p2 * ((t - t1) / (t2 - t1));
            let a3 = p2 * ((t3 - t) / (t3 - t2)) + p3 * ((t - t2) / (t3 - t2));
            let b1 = a1 * ((t2 - t) / t2) + a2 * (t / t2);
            let b2 = a2 * ((t3 - t) / (t3 - t1)) + a3 * ((t - t1) / (t3 - t1));
            out.push(b1 * ((t2 - t) / (t2 - t1)) + b2 * ((t - t1) / (t2 - t1)));
        }
    }
    out.push(if closed { p[0] } else { p[n - 1] });
    out
}

pub fn seg_dist(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let d = b - a;
    let t = if d.length_squared() < 1e-18 { 0.0 } else { ((p - a).dot(d) / d.length_squared()).clamp(0.0, 1.0) };
    (a + d * t - p).length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Units;

    #[test]
    fn rectangle_with_dims() {
        let vars = HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        let mut s = Sketch::default();
        let p: Vec<Id> = [(0.0, 0.0), (10.0, 1.0), (11.0, 7.0), (-1.0, 6.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        let l: Vec<Id> = (0..4).map(|i| s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false)).collect();
        s.constraints.push(Constraint::Horizontal { l: l[0] });
        s.constraints.push(Constraint::Horizontal { l: l[2] });
        s.constraints.push(Constraint::Vertical { l: l[1] });
        s.constraints.push(Constraint::Vertical { l: l[3] });
        s.constraints.push(Constraint::Fix { p: p[0], x: 0.0, y: 0.0 });
        s.constraints.push(Constraint::Length { l: l[0], dim: Dim::new("40", [0.0; 2]) });
        let st = s.solve(&env, &[]);
        assert!(st.ok);
        assert_eq!(st.dof, 1);
        s.constraints.push(Constraint::Length { l: l[1], dim: Dim::new("20", [0.0; 2]) });
        let st = s.solve(&env, &[]);
        assert!(st.ok);
        assert_eq!(st.dof, 0);
        assert!((s.pv(p[2]) - DVec2::new(40.0, 20.0)).length() < 1e-6);
        assert_eq!(st.fixed_curves.len(), 4);
    }

    #[test]
    fn circle_tangent_line() {
        let vars = HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        let mut s = Sketch::default();
        let a = s.add_point(DVec2::new(0.0, 0.0));
        let b = s.add_point(DVec2::new(20.0, 0.0));
        let l = s.add_curve(Geom::Line { a, b }, false);
        let c = s.add_point(DVec2::new(10.0, 7.0));
        let ci = s.add_curve(Geom::Circle { c, r: 3.0 }, false);
        s.constraints.push(Constraint::Tangent { a: l, b: ci, inner: false });
        assert!(s.solve(&env, &[]).ok);
        let r = match s.curve(ci).unwrap().geom {
            Geom::Circle { r, .. } => r,
            _ => 0.0,
        };
        let d = (s.pv(c).y - s.pv(a).y).abs();
        assert!((d - r).abs() < 1e-6, "{d} {r}");
    }

    /// A rectangle fixed at the origin with width and height dimensions: (sketch, lines).
    fn defined_rectangle() -> (Sketch, Vec<Id>) {
        let mut s = Sketch::default();
        let p: Vec<Id> = [(0.0, 0.0), (40.0, 0.0), (40.0, 20.0), (0.0, 20.0)].iter().map(|&(x, y)| s.add_point(DVec2::new(x, y))).collect();
        let l: Vec<Id> = (0..4).map(|i| s.add_curve(Geom::Line { a: p[i], b: p[(i + 1) % 4] }, false)).collect();
        s.constraints.push(Constraint::Horizontal { l: l[0] });
        s.constraints.push(Constraint::Horizontal { l: l[2] });
        s.constraints.push(Constraint::Vertical { l: l[1] });
        s.constraints.push(Constraint::Vertical { l: l[3] });
        s.constraints.push(Constraint::Fix { p: p[0], x: 0.0, y: 0.0 });
        s.constraints.push(Constraint::Length { l: l[0], dim: Dim::new("40", [0.0; 2]) });
        s.constraints.push(Constraint::Length { l: l[1], dim: Dim::new("20", [0.0; 2]) });
        (s, l)
    }

    #[test]
    fn reference_dimensions_measure_without_driving() {
        let vars = HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        let (mut s, l) = defined_rectangle();
        let mut top = Dim::new("40", [0.0; 2]);
        top.driven = true;
        s.constraints.push(Constraint::Length { l: l[2], dim: top });
        assert_eq!(s.solve(&env, &[]).dof, 0);
        // changing the width still solves, and the reference follows
        s.constraints[5].dim_mut().unwrap().expr = "55".into();
        let st = s.solve(&env, &[]);
        assert!(st.ok && st.dof == 0, "{st:?}");
        assert!((s.measure(&s.constraints[7]).unwrap() - 55.0).abs() < 1e-6);
    }

    #[test]
    fn conflicts_name_the_redundant_dimension() {
        let vars = HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        let (mut s, l) = defined_rectangle();
        // a second driving width: any new width now fails, and the culprit is found
        s.constraints.push(Constraint::Length { l: l[2], dim: Dim::new("40", [0.0; 2]) });
        assert!(s.solve(&env, &[]).ok);
        s.constraints[5].dim_mut().unwrap().expr = "50".into();
        assert!(!s.solve(&env, &[]).ok);
        let c = s.conflicts(&env, 5);
        assert!(c.contains(&7), "{c:?}");
        assert!(!c.contains(&4) && !c.contains(&6), "{c:?}");
    }

    #[test]
    fn groups_follow_curves_and_constraints() {
        let mut s = Sketch::default();
        let p: Vec<Id> = (0..4).map(|i| s.add_point(DVec2::new(i as f64, 0.0))).collect();
        let l0 = s.add_curve(Geom::Line { a: p[0], b: p[1] }, false);
        let l1 = s.add_curve(Geom::Line { a: p[2], b: p[3] }, false);
        assert_eq!(s.group_of(l0), [p[0], p[1]].into());
        assert_eq!(s.group_of(p[3]), [p[2], p[3]].into());
        s.constraints.push(Constraint::Parallel { a: l0, b: l1 });
        assert_eq!(s.group_of(p[0]).len(), 4);
    }

    #[test]
    fn equal_distance_keeps_offsets_even() {
        let vars = HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        let mut s = Sketch::default();
        let a = s.add_point(DVec2::new(0.0, 0.0));
        let b = s.add_point(DVec2::new(10.0, 0.0));
        let base = s.add_curve(Geom::Line { a, b }, false);
        let p = s.add_point(DVec2::new(0.0, 3.0));
        let q = s.add_point(DVec2::new(5.0, 7.0));
        s.constraints.push(Constraint::Fix { p: a, x: 0.0, y: 0.0 });
        s.constraints.push(Constraint::Fix { p: b, x: 10.0, y: 0.0 });
        s.constraints.push(Constraint::Fix { p, x: 0.0, y: 3.0 });
        s.constraints.push(Constraint::EqualDistance { p: q, l: base, q: p, m: base });
        assert!(s.solve(&env, &[]).ok);
        assert!((s.pv(q).y.abs() - 3.0).abs() < 1e-6, "{}", s.pv(q));
    }

    #[test]
    fn tangent_at_a_shared_end_counts_once() {
        // a slot: two lines and two arcs, tangent where they meet, equal arcs, axis parallel:
        // position, angle, length and radius are free, so 5 DOF at any size and angle
        let vars = HashMap::new();
        let env = Env { vars: &vars, units: Units::Mm };
        for (scale, ang) in [(1.0, 0.0), (37.0, 2.0), (250.0, 0.7)] {
            let mut s = Sketch::default();
            let at = |v: DVec2| DVec2::from_angle(ang).rotate(v) * scale;
            let (c1, c2, n) = (at(DVec2::ZERO), at(DVec2::new(2.0, 0.0)), at(DVec2::new(0.0, 0.4)));
            let a = s.add_point(c1);
            let b = s.add_point(c2);
            let p1 = s.add_point(c1 + n);
            let p2 = s.add_point(c2 + n);
            let p3 = s.add_point(c2 - n);
            let p4 = s.add_point(c1 - n);
            let l1 = s.add_curve(Geom::Line { a: p1, b: p2 }, false);
            let l2 = s.add_curve(Geom::Line { a: p3, b: p4 }, false);
            let a1 = s.add_curve(Geom::Arc { c: b, a: p3, b: p2 }, false);
            let a2 = s.add_curve(Geom::Arc { c: a, a: p1, b: p4 }, false);
            let axis = s.add_curve(Geom::Line { a, b }, true);
            for (l, ar) in [(l1, a1), (l1, a2), (l2, a1), (l2, a2)] {
                s.constraints.push(Constraint::Tangent { a: l, b: ar, inner: false });
            }
            s.constraints.push(Constraint::Equal { a: a1, b: a2 });
            s.constraints.push(Constraint::Parallel { a: axis, b: l1 });
            let st = s.solve(&env, &[]);
            assert!(st.ok && st.dof == 5, "scale {scale}: {st:?}");
            // one more tangent (to a free circle) takes one more
            let c = s.add_point(at(DVec2::new(1.0, 3.0)));
            let ci = s.add_curve(Geom::Circle { c, r: scale }, false);
            assert_eq!(s.solve(&env, &[]).dof, 8);
            s.constraints.push(Constraint::Tangent { a: l1, b: ci, inner: false });
            assert_eq!(s.solve(&env, &[]).dof, 7);
        }
    }

    #[test]
    fn text_follows_anchor() {
        let mut s = Sketch::default();
        let a = s.add_point(DVec2::new(0.0, 0.0));
        s.texts.push(TextItem { anchor: a, text: "I".into(), height: 10.0, curves: vec![] });
        s.refresh_texts();
        let n = s.curves.len();
        assert!(n >= 1);
        s.point_mut(a).unwrap().x = 50.0;
        s.refresh_texts();
        assert_eq!(s.curves.len(), n, "outlines are replaced, not added");
        let b = s.bounds().unwrap();
        assert!(b.0.x > 45.0, "{b:?}");
    }
}

