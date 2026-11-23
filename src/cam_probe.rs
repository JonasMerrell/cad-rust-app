//! Probing operations: a touch probe finds the stock or the part and sets the work zero (G54) from it:
//! the top (Z), an edge, an outside corner, or the middle of a bore, a boss or a block.
//!
//! `plan` works out the touches in setup coordinates from what the setup knows of the stock and the
//! part. The toolpath is the probe's nominal motion, for the simulation and the cycle time; the post
//! writes the probing cycles from the plan (`Toolpath::probe`), since each touch stops wherever the
//! probe trips.

use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

use crate::cam::{fmt_mm, Operation, ProbeCycle, ProbeOp};
use crate::cam_paths::{inside, Ctx, Pb, Toolpath};

/// Gap the drawn toolpath leaves at each touch, so the simulation doesn't count the touch as a cut.
const TOUCH_GAP: f64 = 0.02;

/// One step of a probing cycle, in setup coordinates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Step {
    /// A positioning move (`feed` None = rapid). With `centre_x`, X is the middle of what two earlier
    /// touches found (their slots), not `to`'s X.
    Move { to: [f64; 3], feed: Option<f64>, centre_x: Option<[usize; 2]> },
    /// Probe along `axis` (0 X, 1 Y, 2 Z) in direction `dir` (±1) from `from`, where the probe is; it should
    /// touch after `expect` mm. The trip position goes into `slot`; the surface is that plus `comp`
    /// (the stylus radius towards the surface for a side, 0 for a top).
    Touch { from: [f64; 3], axis: usize, dir: f64, expect: f64, slot: usize, comp: f64 },
}

/// A work zero axis to set: the surfaces of `touches` (averaged) are at `target` (setup coordinates).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Set {
    pub axis: usize,
    pub touches: Vec<usize>,
    pub target: f64,
}

/// A whole probing cycle.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub sets: Vec<Set>,
    /// How many touches (slots).
    pub touches: usize,
    /// Fast search and slow second-touch feeds, mm/min.
    pub fast: f64,
    pub slow: f64,
    /// Back-off between the two touches, mm.
    pub latch: f64,
    /// How far past the expected surface a touch may go.
    pub overtravel: f64,
    /// What it finds, for the operator: "the front left corner of the stock".
    pub what: String,
}

impl Plan {
    /// Whether an axis comes from more than one touch (a centre): GRBL can't keep the readings to work it out.
    pub fn needs_readings(&self) -> bool {
        self.sets.iter().any(|s| s.touches.len() > 1)
    }

    /// "X0 Y0" in work coordinates.
    pub fn sets_text(&self, origin: DVec3) -> String {
        self.sets.iter().map(|s| format!("{}{}", ["X", "Y", "Z"][s.axis], fmt_mm(((s.target - origin[s.axis]) * 1000.0).round() / 1000.0))).collect::<Vec<_>>().join(" ")
    }

    /// The touches' contact points (setup coordinates, where the stylus ball meets the surface), for drawing.
    pub fn contacts(&self, r: f64) -> Vec<DVec3> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                Step::Touch { from, axis, dir, expect, .. } => {
                    let mut p = DVec3::from(*from);
                    p[*axis] += dir * (expect + if *axis == 2 { 0.0 } else { r });
                    if *axis != 2 {
                        // a side touch meets the wall at the ball's middle
                        p.z += r;
                    }
                    Some(p)
                }
                _ => None,
            })
            .collect()
    }
}

struct B {
    steps: Vec<Step>,
    touches: usize,
    r: f64,
    clr: f64,
    safe: f64,
    above: f64,
    feed: f64,
}

impl B {
    fn rapid(&mut self, p: DVec3) {
        self.steps.push(Step::Move { to: p.to_array(), feed: None, centre_x: None });
    }

    fn feed(&mut self, p: DVec3, cx: Option<[usize; 2]>) {
        self.steps.push(Step::Move { to: p.to_array(), feed: Some(self.feed), centre_x: cx });
    }

    fn touch(&mut self, from: DVec3, axis: usize, dir: f64, expect: f64) -> usize {
        let slot = self.touches;
        self.touches += 1;
        let comp = if axis == 2 { 0.0 } else { dir * self.r };
        self.steps.push(Step::Touch { from: from.to_array(), axis, dir, expect, slot, comp });
        slot
    }

    /// Touch a top at (x, y), `top` its height: down from `clr` above it.
    fn top(&mut self, x: f64, y: f64, top: f64) -> usize {
        self.rapid(DVec3::new(x, y, self.safe));
        let from = DVec3::new(x, y, top + self.clr);
        self.rapid(from);
        let s = self.touch(from, 2, -1.0, self.clr);
        self.rapid(DVec3::new(x, y, self.above.max(top + self.clr)));
        s
    }

    /// Touch a wall from outside: `at` on the wall (its height ignored), probing along `axis` in `dir`, the ball's
    /// tip at `tip_z`. `cx`: X from the middle of two earlier touches.
    fn side(&mut self, at: DVec2, axis: usize, dir: f64, tip_z: f64, cx: Option<[usize; 2]>) -> usize {
        let mut from = DVec3::new(at.x, at.y, tip_z);
        from[axis] -= dir * (self.clr + self.r);
        let over = |z: f64| DVec3::new(from.x, from.y, z);
        match cx {
            Some(_) => {
                self.feed(over(self.above), cx);
                self.feed(from, cx);
            }
            None => {
                self.rapid(over(self.safe));
                self.rapid(over(self.above));
                self.feed(from, None);
            }
        }
        let s = self.touch(from, axis, dir, self.clr);
        self.rapid(over(self.above));
        s
    }
}

/// The highest point of the part straight down at (x, y), if there is any.
fn part_top_at(ctx: &Ctx, x: f64, y: f64) -> Option<f64> {
    let p = DVec2::new(x, y);
    let mut best: Option<f64> = None;
    for t in &ctx.part().tris {
        let (a, b, c) = (t[0].truncate(), t[1].truncate(), t[2].truncate());
        let d = (b - a).perp_dot(c - a);
        if d.abs() < 1e-12 {
            continue;
        }
        let u = (b - p).perp_dot(c - p) / d;
        let v = (c - p).perp_dot(a - p) / d;
        let w = 1.0 - u - v;
        if u < -1e-9 || v < -1e-9 || w < -1e-9 {
            continue;
        }
        let z = t[0].z * u + t[1].z * v + t[2].z * w;
        best = Some(best.map_or(z, |q: f64| q.max(z)));
    }
    best
}

/// A round feature: centre, radius, top, bottom.
#[derive(Clone, Copy, Debug)]
struct Round {
    c: DVec2,
    r: f64,
    top: f64,
    bottom: f64,
}

/// The part's round bosses: vertical cylinders with material inside and none just outside.
fn bosses(ctx: &Ctx) -> Vec<Round> {
    let mut groups: Vec<Round> = vec![];
    for (c, axis, r) in &ctx.part().circles {
        if axis.normalize_or_zero().z.abs() < 0.999 || *r < 0.5 {
            continue;
        }
        match groups.iter_mut().find(|h| (h.c.x - c.x).abs() < 0.01 && (h.c.y - c.y).abs() < 0.01 && (h.r - r).abs() < 0.01) {
            Some(h) => {
                h.top = h.top.max(c.z);
                h.bottom = h.bottom.min(c.z);
            }
            None => groups.push(Round { c: c.truncate(), r: *r, top: c.z, bottom: c.z }),
        }
    }
    groups
        .into_iter()
        .filter(|h| h.top - h.bottom > 0.2)
        .filter(|h| {
            let sh = ctx.geo.shadow((h.top + h.bottom) / 2.0);
            let ring = |d: f64| (0..8).map(move |k| k as f64 * std::f64::consts::FRAC_PI_4).map(move |a| h.c + DVec2::from_angle(a) * d);
            ring(h.r - 0.3).all(|q| inside(&sh, q.to_array())) && ring(h.r + 0.3).all(|q| !inside(&sh, q.to_array()))
        })
        .collect()
}

/// The one nearest `at`, or the biggest.
fn pick(v: Vec<Round>, at: Option<DVec2>) -> Option<Round> {
    match at {
        Some(p) => v.into_iter().min_by(|a, b| a.c.distance(p).partial_cmp(&b.c.distance(p)).unwrap()),
        None => v.into_iter().max_by(|a, b| a.r.partial_cmp(&b.r).unwrap()),
    }
}

fn corner_signs(side: &str) -> Result<(f64, f64, &'static str), String> {
    Ok(match side.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
        "front_left" | "left_front" | "fl" => (-1.0, -1.0, "front left"),
        "front_right" | "right_front" | "fr" => (1.0, -1.0, "front right"),
        "back_left" | "left_back" | "bl" | "rear_left" => (-1.0, 1.0, "back left"),
        "back_right" | "right_back" | "br" | "rear_right" => (1.0, 1.0, "back right"),
        s => return Err(format!("corner \"{s}\": front_left, front_right, back_left or back_right")),
    })
}

/// The probing cycle for `op` with a stylus ball of radius `r`, the probe's fast and slow feeds.
pub fn plan(ctx: &Ctx, op: &Operation, r: f64, fast: f64, slow: f64) -> Result<(Plan, Vec<String>), String> {
    let p: ProbeOp = op.probing.clone().unwrap_or_default();
    let mut warnings = vec![];
    let clr = p.clearance.max(0.5);
    let at = op.at.map(|a| DVec2::new(a[0], a[1]) + ctx.origin.truncate());
    let (slo, shi) = ctx.stock;
    let part = ctx.part();
    let (lo, hi, of) = if p.on_part { (part.lo, part.hi, "the part") } else { (slo, shi, "the stock") };
    let mut b = B { steps: vec![], touches: 0, r, clr, safe: ctx.safe(), above: 0.0, feed: (fast * 4.0).clamp(fast.max(1.0), 2000.0) };
    let retract = ctx.cam.retract.max(1.0);
    // the ball's tip `depth` below a top: it touches walls at its middle, one radius higher
    let tip_below = |top: f64, bottom: f64, b: &B| -> f64 {
        let want = top - p.depth.max(0.1) - r;
        want.max(bottom + 0.3).min(top - r * 0.5).min(b.above)
    };
    let mut sets = vec![];
    let what;
    match p.cycle {
        ProbeCycle::Z => {
            let xy = at.unwrap_or(((lo + hi) / 2.0).truncate());
            let top = if p.on_part {
                part_top_at(ctx, xy.x, xy.y)
                    .ok_or_else(|| format!("there is no part under X{} Y{}: give the point to touch the top at", fmt_mm(xy.x - ctx.origin.x), fmt_mm(xy.y - ctx.origin.y)))?
            } else {
                if xy.x < lo.x || xy.x > hi.x || xy.y < lo.y || xy.y > hi.y {
                    return Err("the point to touch is off the stock".into());
                }
                shi.z
            };
            b.above = top + retract.max(clr);
            let s = b.top(xy.x, xy.y, top);
            sets.push(Set { axis: 2, touches: vec![s], target: top });
            what = format!("the top of {of}");
        }
        ProbeCycle::Edge => {
            let side = p.side.trim().to_lowercase();
            let (axis, dir, name) = match side.as_str() {
                "left" | "x-" | "-x" => (0, 1.0, "left"),
                "right" | "x+" | "+x" => (0, -1.0, "right"),
                "front" | "y-" | "-y" => (1, 1.0, "front"),
                "back" | "rear" | "y+" | "+y" => (1, -1.0, "back"),
                s => return Err(format!("edge \"{s}\": left, right, front or back")),
            };
            let surface = if dir > 0.0 { lo[axis] } else { hi[axis] };
            let mid = ((lo + hi) / 2.0).truncate();
            let mut pt = at.unwrap_or(mid);
            pt[axis] = surface;
            let top = if p.on_part {
                let mut q = pt;
                q[axis] += dir * 0.5;
                part_top_at(ctx, q.x, q.y).unwrap_or(hi.z)
            } else {
                hi.z
            };
            b.above = top + retract;
            let s = b.side(pt, axis, dir, tip_below(top, lo.z, &b), None);
            sets.push(Set { axis, touches: vec![s], target: surface });
            what = format!("the {name} edge of {of}");
        }
        ProbeCycle::Corner => {
            let (sx, sy, name) = corner_signs(&p.side)?;
            let c = DVec2::new(if sx < 0.0 { lo.x } else { hi.x }, if sy < 0.0 { lo.y } else { hi.y });
            let size = (hi - lo).truncate();
            let inset = p.inset.max(r + 0.5).min(size.x.min(size.y) * 0.45);
            if inset < p.inset - 1e-6 {
                warnings.push(format!("touches the sides {} mm from the corner (the block is too small for {})", fmt_mm((inset * 10.0).round() / 10.0), fmt_mm(p.inset)));
            }
            let inner = DVec2::new(c.x - sx * inset, c.y - sy * inset);
            let top = if p.on_part { part_top_at(ctx, inner.x, inner.y).unwrap_or(hi.z) } else { hi.z };
            b.above = top + retract.max(clr);
            if p.with_z {
                let s = b.top(inner.x, inner.y, top);
                sets.push(Set { axis: 2, touches: vec![s], target: top });
            }
            let tip = tip_below(top, lo.z, &b);
            let sx_ = b.side(DVec2::new(c.x, inner.y), 0, -sx, tip, None);
            let sy_ = b.side(DVec2::new(inner.x, c.y), 1, -sy, tip, None);
            sets.insert(0, Set { axis: 1, touches: vec![sy_], target: c.y });
            sets.insert(0, Set { axis: 0, touches: vec![sx_], target: c.x });
            what = format!("the {name} corner of {of}");
        }
        ProbeCycle::Bore => {
            let hole = if p.size > 0.0 {
                let c = at.ok_or("give the bore's middle (at) with its diameter")?;
                // a bore in the stock beside the part (a carrier between parts): its top is the stock's, not the part's
                let top = part_top_at(ctx, c.x + p.size / 2.0 + 1.0, c.y).or_else(|| part_top_at(ctx, c.x - p.size / 2.0 - 1.0, c.y)).unwrap_or(ctx.stock.1.z);
                Round { c, r: p.size / 2.0, top, bottom: part.lo.z }
            } else {
                let holes: Vec<Round> = ctx.geo.holes().into_iter().map(|h| Round { c: DVec2::new(h.x, h.y), r: h.r, top: h.top, bottom: h.bottom }).collect();
                pick(holes, at).ok_or("the part has no round hole to find the middle of: give the bore's middle (at) and diameter (size)")?
            };
            if hole.r - r < 0.3 {
                return Err(format!("the Ø{} stylus ball doesn't fit the Ø{} bore", fmt_mm(r * 2.0), fmt_mm(hole.r * 2.0)));
            }
            b.above = hole.top + retract;
            let tip = tip_below(hole.top, hole.bottom, &b);
            let c3 = DVec3::new(hole.c.x, hole.c.y, tip);
            b.rapid(DVec3::new(c3.x, c3.y, b.safe));
            b.rapid(DVec3::new(c3.x, c3.y, b.above));
            b.feed(c3, None);
            let reach = hole.r - r;
            let x0 = b.touch(c3, 0, -1.0, reach);
            let x1 = b.touch(c3, 0, 1.0, reach);
            b.feed(c3, Some([x0, x1]));
            let y0 = b.touch(c3, 1, -1.0, reach);
            let y1 = b.touch(c3, 1, 1.0, reach);
            b.feed(DVec3::new(c3.x, c3.y, b.above), Some([x0, x1]));
            sets.push(Set { axis: 0, touches: vec![x0, x1], target: hole.c.x });
            sets.push(Set { axis: 1, touches: vec![y0, y1], target: hole.c.y });
            what = format!("the middle of the Ø{} bore at X{} Y{}", fmt_mm((hole.r * 2000.0).round() / 1000.0), fmt_mm(hole.c.x - ctx.origin.x), fmt_mm(hole.c.y - ctx.origin.y));
        }
        ProbeCycle::Boss => {
            // a round boss of the part, or the block (stock, or the part's outline)
            let found = if p.size > 0.0 {
                let c = at.ok_or("give the boss's middle (at) with its diameter")?;
                Some(Round { c, r: p.size / 2.0, top: part_top_at(ctx, c.x, c.y).unwrap_or(hi.z), bottom: lo.z })
            } else if p.on_part {
                pick(bosses(ctx), at)
            } else {
                None
            };
            let (c, half, top, bottom, name) = match found {
                Some(h) => (h.c, DVec2::splat(h.r), h.top, h.bottom, format!("the middle of the Ø{} boss", fmt_mm((h.r * 2000.0).round() / 1000.0))),
                None => (((lo + hi) / 2.0).truncate(), ((hi - lo) / 2.0).truncate(), hi.z, lo.z, format!("the middle of {of}")),
            };
            b.above = top + retract;
            let tip = tip_below(top, bottom, &b);
            let x0 = b.side(DVec2::new(c.x - half.x, c.y), 0, 1.0, tip, None);
            let x1 = b.side(DVec2::new(c.x + half.x, c.y), 0, -1.0, tip, None);
            let y0 = b.side(DVec2::new(c.x, c.y - half.y), 1, 1.0, tip, Some([x0, x1]));
            let y1 = b.side(DVec2::new(c.x, c.y + half.y), 1, -1.0, tip, Some([x0, x1]));
            sets.push(Set { axis: 0, touches: vec![x0, x1], target: c.x });
            sets.push(Set { axis: 1, touches: vec![y0, y1], target: c.y });
            what = name;
        }
    }
    let plan = Plan { touches: b.touches, steps: b.steps, sets, fast: fast.max(1.0), slow: slow.max(1.0), latch: 1.0, overtravel: p.overtravel.max(0.5), what };
    Ok((plan, warnings))
}

/// The probing operation's toolpath: the probe's nominal motion (each touch stops a hair short of the surface).
pub(crate) fn probe(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let tool = pb.tool;
    let (plan, warnings) = match plan(ctx, op, tool.radius(), tool.feed, tool.plunge_feed) {
        Ok(p) => p,
        Err(e) => {
            tp.warnings.push(e);
            return;
        }
    };
    tp.warnings.extend(warnings);
    let (p_on_part, p_cycle) = op.probing.as_ref().map(|p| (p.on_part, p.cycle)).unwrap_or((false, ProbeCycle::Z));
    match ctx.cam.post.as_str() {
        "grbl" if plan.needs_readings() => tp.warnings.push("GRBL can't keep probe readings to work out a middle: find it by hand, or use an edge or corner".into()),
        // the stock's top or middle posts as rcnc's Find stock, which keeps that top as the Z every later tool
        // is measured to
        "rcnc" if plan.sets.iter().any(|s| s.axis == 2) && !(!p_on_part && matches!(p_cycle, ProbeCycle::Z | ProbeCycle::Boss)) => tp.warnings.push(
            "rcnc measures each tool on the tool setter at the next tool change and sets the work Z from it, replacing this Z: \
             leave Z to rcnc (or turn its tool change touch-off off)"
                .into(),
        ),
        _ if ctx.cam.probe.mode == "plate" && plan.sets.iter().any(|s| s.axis == 2) => {
            tp.warnings.push("the touch plate sets the work Z again after every tool change, replacing this Z".into())
        }
        _ => {}
    }
    let mut pos: Option<DVec3> = None;
    for s in &plan.steps {
        match s {
            Step::Move { to, feed, .. } => {
                let p = DVec3::from(*to);
                match feed {
                    Some(f) => pb.cut(p, *f),
                    None => pb.rapid(p),
                }
                pos = Some(p);
            }
            Step::Touch { from, axis, dir, expect, .. } => {
                let from = DVec3::from(*from);
                if pos.map(|q| q.distance(from) > 1e-6).unwrap_or(true) {
                    pb.cut(from, plan.fast);
                }
                let mut hit = from;
                hit[*axis] += dir * (expect - TOUCH_GAP).max(0.0);
                pb.cut(hit, plan.fast);
                pb.cut(from, plan.fast);
                pos = Some(from);
            }
        }
    }
    tp.probe = Some(plan);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::{Cam, OpKind, Tool, ToolKind};
    use crate::cam_paths::{generate, Part};

    /// A 40 × 30 × 10 plate.
    fn plate() -> Part {
        let mut tris = vec![];
        let quad = |t: &mut Vec<[DVec3; 3]>, a: DVec3, b: DVec3, c: DVec3, d: DVec3| {
            t.push([a, b, c]);
            t.push([a, c, d]);
        };
        let (x0, y0, x1, y1, z1) = (0.0, 0.0, 40.0, 30.0, 10.0);
        let v = |x: f64, y: f64, z: f64| DVec3::new(x, y, z);
        quad(&mut tris, v(x0, y0, z1), v(x1, y0, z1), v(x1, y1, z1), v(x0, y1, z1));
        quad(&mut tris, v(x0, y0, 0.0), v(x0, y1, 0.0), v(x1, y1, 0.0), v(x1, y0, 0.0));
        quad(&mut tris, v(x0, y0, 0.0), v(x1, y0, 0.0), v(x1, y0, z1), v(x0, y0, z1));
        quad(&mut tris, v(x1, y0, 0.0), v(x1, y1, 0.0), v(x1, y1, z1), v(x1, y0, z1));
        quad(&mut tris, v(x1, y1, 0.0), v(x0, y1, 0.0), v(x0, y1, z1), v(x1, y1, z1));
        quad(&mut tris, v(x0, y1, 0.0), v(x0, y0, 0.0), v(x0, y0, z1), v(x0, y1, z1));
        Part::new(&tris, &[], "top")
    }

    fn setup(post: &str) -> Cam {
        let mut c = Cam { post: post.into(), ..Default::default() };
        c.stock.side = 5.0;
        c.stock.top = 1.0;
        let probe = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Probe).unwrap();
        assert_eq!(probe.rpm, 0.0);
        c.add_tool(probe);
        c
    }

    fn probe_op(c: &mut Cam, f: impl FnOnce(&mut Operation)) -> u64 {
        let id = c.add_op(OpKind::Probe, 0);
        f(c.op_mut(id).unwrap());
        id
    }

    #[test]
    fn corner_touches_both_sides_from_outside_and_sets_the_work_zero() {
        let part = plate();
        let mut c = setup("mach3");
        // work zero at the stock's top front left corner (the default)
        let id = probe_op(&mut c, |o| {
            let p = o.probing.as_mut().unwrap();
            p.cycle = ProbeCycle::Corner;
            p.side = "front_left".into();
            p.with_z = true;
        });
        let paths = generate(&c, &part);
        let tp = paths.iter().find(|p| p.op == id).unwrap();
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        let plan = tp.probe.as_ref().unwrap();
        let ctx = crate::cam_paths::Ctx::new(&c, &part);
        assert_eq!(plan.sets_text(ctx.origin), "X0 Y0 Z0");
        assert_eq!(plan.touches, 3);
        // the side touches go +X and +Y from outside the stock, below its top
        let touches: Vec<_> = plan.steps.iter().filter_map(|s| if let Step::Touch { from, axis, dir, .. } = s { Some((*from, *axis, *dir)) } else { None }).collect();
        let (lo, hi) = ctx.stock;
        assert_eq!((touches[1].1, touches[1].2), (0, 1.0));
        assert!(touches[1].0[0] < lo.x - 1.0 && touches[1].0[2] < hi.z - 3.0);
        assert_eq!((touches[2].1, touches[2].2), (1, 1.0));
        assert!(touches[2].0[1] < lo.y - 1.0);
        let g = crate::cam_post::gcode(&c, &paths, ctx.origin, ctx.safe());
        assert!(g.contains("G31 X") && g.contains("G31 Y") && g.contains("G31 Z"), "{g}");
        assert!(g.contains("#110 = #2002") && g.contains("#111 = #2000") && g.contains("#112 = #2001"), "{g}");
        assert!(g.contains("G10 L2 P1 X[#5221 + #111 + 1] Y[#5222 + #112 + 1] Z[#5223 + #110]"), "{g}");
        // the probe doesn't spin
        let probing = &g[g.find("(Probe top").unwrap()..];
        assert!(!probing.contains(" M3") && !probing.contains("M8"), "{probing}");
        // the simulation finds nothing wrong: every touch stops short of the stock
        let sim = crate::cam_sim::Sim::new(&c, &part, ctx.stock, &paths, 200);
        let mut sim = sim;
        sim.run_to(usize::MAX, usize::MAX);
        assert!(sim.events.is_empty(), "{:?}", sim.events);
        assert!(sim.removed < 1.0, "{}", sim.removed);
    }

    #[test]
    fn block_centre_touches_are_not_crashes_at_fine_resolution() {
        // a 39 × 77 block: its sides fall between the stock map's cells, which a touch that stops a hair
        // short of them must not read as running into the work
        let part = plate();
        for res in [200, 500, 777] {
            let mut c = setup("rcnc");
            c.stock.size = Some([39.0, 77.0, 20.0]);
            probe_op(&mut c, |o| o.probing.as_mut().unwrap().cycle = ProbeCycle::Boss);
            let paths = generate(&c, &part);
            let ctx = crate::cam_paths::Ctx::new(&c, &part);
            let mut sim = crate::cam_sim::Sim::new(&c, &part, ctx.stock, &paths, res);
            sim.run_to(usize::MAX, usize::MAX);
            assert!(sim.events.is_empty(), "{res}: {:?}", sim.events);
        }
    }

    #[test]
    fn grbl_sets_each_axis_where_the_probe_stopped() {
        let part = plate();
        let mut c = setup("grbl");
        let id = probe_op(&mut c, |o| {
            let p = o.probing.as_mut().unwrap();
            p.cycle = ProbeCycle::Edge;
            p.side = "right".into();
        });
        let paths = generate(&c, &part);
        let (origin, safe) = {
            let ctx = crate::cam_paths::Ctx::new(&c, &part);
            (ctx.origin, ctx.safe())
        };
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        // stock 50 wide from the corner: the right edge is X50, where the stylus centre stops 1 mm past it
        assert!(g.contains("G38.2 X") && g.contains("G10 L20 P1 X51"), "{g}");
        assert!(!g.contains('#'), "{g}");
        // a middle needs the readings
        c.op_mut(id).unwrap().probing.as_mut().unwrap().cycle = ProbeCycle::Boss;
        let paths = generate(&c, &part);
        assert!(paths[0].warnings.iter().any(|w| w.contains("GRBL")), "{:?}", paths[0].warnings);
        let g = crate::cam_post::gcode(&c, &paths, origin, safe);
        assert!(g.contains("M0") && !g.contains("G38.2"), "{g}");
    }

    #[test]
    fn block_centre_on_linuxcnc_averages_opposite_touches() {
        let part = plate();
        let mut c = setup("linuxcnc");
        c.origin = crate::cam::Origin::StockTopCenter;
        let id = probe_op(&mut c, |o| o.probing.as_mut().unwrap().cycle = ProbeCycle::Boss);
        let paths = generate(&c, &part);
        let tp = paths.iter().find(|p| p.op == id).unwrap();
        assert!(tp.warnings.is_empty(), "{:?}", tp.warnings);
        let ctx = crate::cam_paths::Ctx::new(&c, &part);
        assert_eq!(tp.probe.as_ref().unwrap().sets_text(ctx.origin), "X0 Y0");
        let g = crate::cam_post::gcode(&c, &paths, ctx.origin, ctx.safe());
        assert!(g.contains("G38.2 X") && g.contains("#110 = #5061") && g.contains("#113 = #5062"), "{g}");
        assert!(g.contains("G10 L2 P1 X[#5221 + [#110 + 1 + #111 - 1] / 2] Y[#5222 + [#112 + 1 + #113 - 1] / 2]"), "{g}");
        // the Y touches go over at the X middle the first two found
        assert!(g.contains("X[[#110 + #111] / 2]"), "{g}");
        let mut sim = crate::cam_sim::Sim::new(&c, &part, ctx.stock, &paths, 200);
        sim.run_to(usize::MAX, usize::MAX);
        assert!(sim.events.is_empty(), "{:?}", sim.events);
    }

    #[test]
    fn bore_centre_from_a_given_size_and_too_small_bores() {
        let part = plate();
        let mut c = setup("rcnc");
        let id = probe_op(&mut c, |o| {
            o.at = Some([25.0, 20.0]);
            let p = o.probing.as_mut().unwrap();
            p.cycle = ProbeCycle::Bore;
            p.size = 12.0;
        });
        let paths = generate(&c, &part);
        let tp = paths.iter().find(|p| p.op == id).unwrap();
        let plan = tp.probe.as_ref().expect("plan");
        assert_eq!(plan.touches, 4);
        assert!(plan.what.contains("Ø12 bore"), "{}", plan.what);
        // each touch reaches the wall: 6 - 1 = 5 mm from the middle
        assert!(plan.steps.iter().all(|s| !matches!(s, Step::Touch { expect, .. } if (expect - 5.0).abs() > 1e-9)));
        let ctx = crate::cam_paths::Ctx::new(&c, &part);
        let g = crate::cam_post::gcode(&c, &paths, ctx.origin, ctx.safe());
        assert!(g.contains("G10 L2 P1 X[#5221 + [#110 - 1 + #111 + 1] / 2 - 25] Y[#5222 + [#112 - 1 + #113 + 1] / 2 - 20]"), "{g}");
        // a Ø1.5 bore is too small for the Ø2 ball
        c.op_mut(id).unwrap().probing.as_mut().unwrap().size = 1.5;
        let paths = generate(&c, &part);
        assert!(paths[0].probe.is_none() && paths[0].warnings.iter().any(|w| w.contains("doesn't fit")), "{:?}", paths[0].warnings);
    }

    #[test]
    fn a_bore_beside_the_part_is_probed_below_the_stocks_top() {
        // a locating bore in the stock beside the part (a carrier between two parts): its top is the stock's top,
        // not the part's (the probe went the stock-above-part height too deep)
        let part = plate();
        let mut c = setup("rcnc");
        c.stock.side = 20.0;
        c.stock.top = 3.0;
        let (o, stock_top) = {
            let ctx = crate::cam_paths::Ctx::new(&c, &part);
            (ctx.origin, ctx.stock.1.z)
        };
        let at = DVec3::new(part.lo.x - 10.0, (part.lo.y + part.hi.y) / 2.0, 0.0) - o;
        let id = probe_op(&mut c, |op| {
            op.at = Some([at.x, at.y]);
            let p = op.probing.as_mut().unwrap();
            p.cycle = ProbeCycle::Bore;
            p.size = 8.0;
            p.depth = 1.0;
        });
        let paths = generate(&c, &part);
        let tp = paths.iter().find(|p| p.op == id).unwrap();
        let plan = tp.probe.as_ref().expect("plan");
        let z = plan.steps.iter().find_map(|s| if let Step::Touch { from, .. } = s { Some(from[2]) } else { None }).unwrap();
        let ball = c.tools[0].diameter / 2.0;
        assert!((z - (stock_top - 1.0 - ball)).abs() < 1e-6, "touches at {z}, stock top {stock_top}, part top {}", part.hi.z);
    }

    #[test]
    fn z_on_rcnc_warns_that_the_tool_change_resets_it() {
        let part = plate();
        let mut c = setup("rcnc");
        // the stock's top is rcnc's Find stock, which keeps it as the Z later tools are measured to: no warning
        probe_op(&mut c, |_| {});
        let paths = generate(&c, &part);
        assert!(paths[0].warnings.is_empty(), "{:?}", paths[0].warnings);
        c.ops.clear();
        probe_op(&mut c, |o| o.probing.as_mut().unwrap().on_part = true);
        let mut cutter = Tool::default();
        cutter.number = 0;
        let t = c.add_tool(cutter);
        c.add_op(OpKind::Face, t);
        let paths = generate(&c, &part);
        assert!(paths[0].warnings.iter().any(|w| w.contains("rcnc")), "{:?}", paths[0].warnings);
        let ctx = crate::cam_paths::Ctx::new(&c, &part);
        let g = crate::cam_post::gcode(&c, &paths, ctx.origin, ctx.safe());
        // the probe goes in and out with M6; the face mill then starts its spindle
        assert!(g.contains("T1 M6") && g.contains("T2 M6") && g.contains("G10 L2 P1 Z[#5223 + #110"), "{g}");
        let before_face = &g[..g.find("T2 M6").unwrap()];
        assert!(!before_face.contains(" M3"), "{before_face}");
    }
}
