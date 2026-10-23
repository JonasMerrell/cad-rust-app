//! Operations on sketch curves: trace (follow them at a depth) and V-carve (carve their closed shapes
//! with a V-bit, as deep as each point's distance to the outline allows).

use glam::{DVec2, DVec3};

use crate::cam::{Operation, Side, Tool, ToolKind};
use crate::cam_3d::{Dropper, Scratch};
use crate::cam_paths::{even_odd, flat, inside, offset, stepdown, stepover, Contour, Ctx, Pb, Shapes, Toolpath};

/// Points closer than this join up into one path.
const JOIN: f64 = 1e-3;
/// Spacing of the points along a path that follows the surface.
const SAMPLE: f64 = 0.5;

/// The curves of the operation's sketches in setup XY: closed loops, and open paths chained end to end.
fn curves(ctx: &Ctx, op: &Operation) -> (Vec<Vec<DVec2>>, Vec<Vec<DVec2>>, Vec<u64>) {
    let mut closed = vec![];
    let mut open: Vec<Vec<DVec2>> = vec![];
    let mut missing = vec![];
    for id in &op.sketches {
        let Some(sk) = ctx.part().sketches.iter().find(|s| s.id == *id) else {
            missing.push(*id);
            continue;
        };
        for (pts, is_closed) in &sk.paths {
            let mut p: Vec<DVec2> = pts.iter().map(|q| q.truncate()).collect();
            p.dedup_by(|a, b| a.distance(*b) < JOIN);
            if p.len() < 2 {
                continue;
            }
            if *is_closed || (p.len() > 2 && p[0].distance(*p.last().unwrap()) < JOIN) {
                if p[0].distance(*p.last().unwrap()) < JOIN {
                    p.pop();
                }
                if p.len() >= 3 {
                    closed.push(p);
                }
            } else {
                open.push(p);
            }
        }
    }
    // chain the open curves (lines and arcs drawn one after another)
    let mut chains: Vec<Vec<DVec2>> = vec![];
    while let Some(mut c) = open.pop() {
        loop {
            let (a, b) = (c[0], *c.last().unwrap());
            let Some((i, rev, at_end)) = open.iter().enumerate().find_map(|(i, o)| {
                let (oa, ob) = (o[0], *o.last().unwrap());
                if b.distance(oa) < JOIN {
                    Some((i, false, true))
                } else if b.distance(ob) < JOIN {
                    Some((i, true, true))
                } else if a.distance(ob) < JOIN {
                    Some((i, false, false))
                } else if a.distance(oa) < JOIN {
                    Some((i, true, false))
                } else {
                    None
                }
            }) else {
                break;
            };
            let mut o = open.remove(i);
            if rev {
                o.reverse();
            }
            if at_end {
                c.extend(o.into_iter().skip(1));
            } else {
                o.pop();
                o.extend(c);
                c = o;
            }
        }
        if c.len() > 3 && c[0].distance(*c.last().unwrap()) < JOIN {
            c.pop();
            closed.push(c);
        } else {
            chains.push(c);
        }
    }
    (closed, chains, missing)
}

/// Height of the part's surface (or else the top of the stock) under a point.
struct Surface {
    drop: Dropper,
    top: f64,
    scratch: Scratch,
}

impl Surface {
    fn new(ctx: &Ctx) -> Surface {
        let probe = Tool { kind: ToolKind::Flat, diameter: 0.02, ..Default::default() };
        Surface { drop: Dropper::new(ctx.part(), &probe, 0.0), top: ctx.stock.1.z, scratch: Scratch::default() }
    }

    fn z(&mut self, p: DVec2) -> f64 {
        let z = self.drop.drop(p, &mut self.scratch).0;
        if z.is_finite() {
            z
        } else {
            self.top
        }
    }

    /// The path `pts` (closed: back to the start) `depth` below the surface, sampled finely enough to follow it.
    fn path(&mut self, pts: &[DVec2], closed: bool, depth: f64) -> Vec<DVec3> {
        let n = pts.len();
        let segs = if closed { n } else { n - 1 };
        let mut out: Vec<DVec3> = vec![];
        for i in 0..segs {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let k = ((a.distance(b) / SAMPLE).ceil() as usize).max(1);
            for j in 0..k {
                let q = a.lerp(b, j as f64 / k as f64);
                out.push(q.extend(self.z(q) - depth));
            }
        }
        let last = if closed { pts[0] } else { pts[n - 1] };
        out.push(last.extend(self.z(last) - depth));
        // drop the samples that lie on a straight line anyway
        let mut keep: Vec<DVec3> = vec![out[0]];
        for i in 1..out.len() - 1 {
            let (a, b, c) = (*keep.last().unwrap(), out[i], out[i + 1]);
            let ac = c - a;
            let t = ((b - a).dot(ac) / ac.length_squared().max(1e-18)).clamp(0.0, 1.0);
            if (a + ac * t).distance(b) > 1e-4 {
                keep.push(b);
            }
        }
        keep.push(out[out.len() - 1]);
        keep
    }
}

/// Feed along a path; stays down when it starts within `link` of where the tool is (in XY, and inside `region`).
fn follow(pb: &mut Pb, ctx: &Ctx, pts: &[DVec3], link: f64, region: Option<&Shapes>) {
    let Some(&first) = pts.first() else { return };
    let stay = |p: DVec3| {
        p.z < pb.safe - 1e-6
            && p.truncate().distance(first.truncate()) <= link
            && region.map(|r| (1..4).all(|k| inside(r, p.truncate().lerp(first.truncate(), k as f64 / 4.0).to_array()))).unwrap_or(true)
    };
    match pb.pos {
        Some(p) if stay(p) => pb.cut(first, pb.plunge),
        _ => {
            pb.approach(first.x, first.y, ctx.stock.1.z.max(first.z));
            pb.cut(first, pb.plunge);
        }
    }
    for p in &pts[1..] {
        pb.cut(*p, pb.feed);
    }
}

/// Start a closed loop at the point nearest the tool.
fn from_nearest(l: &[DVec2], pos: Option<DVec3>) -> Vec<DVec2> {
    let Some(p) = pos else { return l.to_vec() };
    let i = (0..l.len()).min_by(|a, b| l[*a].distance_squared(p.truncate()).total_cmp(&l[*b].distance_squared(p.truncate()))).unwrap_or(0);
    l[i..].iter().chain(&l[..i]).copied().collect()
}

fn to_contour(l: &[DVec2]) -> Contour {
    l.iter().map(|p| [p.x, p.y]).collect()
}

fn loops_of(s: &Shapes, climb: bool) -> Vec<Vec<DVec2>> {
    flat(s).into_iter().map(|c| if climb { c.iter().rev().map(|p| DVec2::from(*p)).collect() } else { c.iter().map(|p| DVec2::from(*p)).collect() }).collect()
}

fn check_sketches(op: &Operation, missing: &[u64], tp: &mut Toolpath) -> bool {
    if op.sketches.is_empty() {
        tp.warnings.push("pick the sketches to follow".into());
        return false;
    }
    if !missing.is_empty() {
        tp.warnings.push(format!("{} of the sketches {} gone", missing.len(), if missing.len() == 1 { "is" } else { "are" }));
    }
    true
}

pub(crate) fn trace(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let (closed, open, missing) = curves(ctx, op);
    if !check_sketches(op, &missing, tp) {
        return;
    }
    if closed.is_empty() && open.is_empty() {
        tp.warnings.push("the sketches have no curves".into());
        return;
    }
    let t = pb.tool;
    let depth = op.depth.max(0.0);
    let passes = ((depth / stepdown(op, t)).ceil() as usize).max(1);
    let mut surf = Surface::new(ctx);
    // closed shapes: on the line, or beside them
    let loops: Vec<Vec<DVec2>> = match op.side {
        Side::Both => closed.into_iter().map(|l| if op.climb { l.into_iter().rev().collect() } else { l }).collect(),
        side => {
            let region = even_odd(&closed.iter().map(|l| to_contour(l)).collect::<Vec<_>>());
            let d = t.radius() + op.leave;
            loops_of(&offset(&region, if side == Side::Outside { d } else { -d }), op.climb)
        }
    };
    if loops.is_empty() && open.is_empty() {
        tp.warnings.push("the tool doesn't fit inside the shapes".into());
        return;
    }
    let mut todo: Vec<(Vec<DVec2>, bool)> = loops.into_iter().map(|l| (l, true)).chain(open.into_iter().map(|l| (l, false))).collect();
    while !todo.is_empty() {
        let here = pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
        let near = |l: &(Vec<DVec2>, bool)| {
            if l.1 {
                l.0.iter().map(|p| p.distance(here)).fold(f64::INFINITY, f64::min)
            } else {
                l.0[0].distance(here).min(l.0.last().unwrap().distance(here))
            }
        };
        let j = (0..todo.len()).min_by(|a, b| near(&todo[*a]).total_cmp(&near(&todo[*b]))).unwrap();
        let (mut l, is_closed) = todo.remove(j);
        if is_closed {
            l = from_nearest(&l, pb.pos);
        } else if l.last().unwrap().distance(here) < l[0].distance(here) {
            l.reverse();
        }
        // pass by pass down; open paths go back and forth
        for k in 1..=passes {
            let d = depth * k as f64 / passes as f64;
            let pts = surf.path(&l, is_closed, d);
            follow(pb, ctx, &pts, JOIN, None);
            if !is_closed {
                l.reverse();
            }
        }
        pb.up();
    }
}

pub(crate) fn vcarve(ctx: &Ctx, op: &Operation, pb: &mut Pb, tp: &mut Toolpath) {
    let (closed, _, missing) = curves(ctx, op);
    if !check_sketches(op, &missing, tp) {
        return;
    }
    let region = even_odd(&closed.iter().map(|l| to_contour(l)).collect::<Vec<_>>());
    if region.is_empty() {
        tp.warnings.push("the sketches have no closed shapes to carve".into());
        return;
    }
    let t = pb.tool;
    let tan = (t.angle.clamp(10.0, 170.0) / 2.0).to_radians().tan();
    let tip = t.tip_diameter.max(0.0) / 2.0;
    let cone = ((t.radius() - tip) / tan).max(0.0);
    if op.depth > cone + 1e-6 {
        tp.warnings.push(format!("the V-bit's cone is only {} mm deep", crate::cam::fmt_mm(cone)));
    }
    let hmax = if op.depth > 0.0 { op.depth.min(cone) } else { cone };
    let s = stepover(op, t).max(0.01);
    let mut surf = Surface::new(ctx);
    // contour-parallel passes in from the outline: at distance d the cone touches the outline at the surface
    let mut d = tip + s * 0.5;
    let mut n = 0;
    let mut flat_floor = false;
    loop {
        let h = ((d - tip) / tan).clamp(0.0, hmax);
        flat_floor |= (d - tip) / tan > hmax + 1e-9;
        let sh = offset(&region, -d);
        if sh.is_empty() {
            break;
        }
        let mut loops = loops_of(&sh, op.climb);
        while !loops.is_empty() {
            let here = pb.pos.map(|p| p.truncate()).unwrap_or(DVec2::ZERO);
            let dist = |l: &Vec<DVec2>| l.iter().map(|p| p.distance_squared(here)).fold(f64::INFINITY, f64::min);
            let j = (0..loops.len()).min_by(|a, b| dist(&loops[*a]).total_cmp(&dist(&loops[*b]))).unwrap();
            let l = from_nearest(&loops.remove(j), pb.pos);
            let pts = surf.path(&l, true, h);
            follow(pb, ctx, &pts, s * 1.5, Some(&region));
        }
        d += s;
        n += 1;
        if n > 4000 {
            tp.warnings.push("too many passes: use a bigger stepover or a smaller shape".into());
            break;
        }
    }
    if flat_floor && tip <= 0.0 {
        tp.warnings.push(format!("the wide parts are carved {} mm deep with a ridged floor; clear them with a flat end mill", crate::cam::fmt_mm(hmax)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::{Cam, OpKind};
    use crate::cam_paths::{generate, Move, Part, SketchPaths};
    use crate::cam_sim::{EventKind, Sim};

    /// A closed 40 × 30 × 10 box with a sketch on its top.
    fn block(paths: Vec<(Vec<DVec3>, bool)>) -> Part {
        let v = |x: f64, y: f64, z: f64| DVec3::new(x * 40.0, y * 30.0, z * 10.0);
        let q = |a: DVec3, b: DVec3, c: DVec3, d: DVec3| [[a, b, c], [a, c, d]];
        let mut t = vec![];
        t.extend(q(v(0., 0., 1.), v(1., 0., 1.), v(1., 1., 1.), v(0., 1., 1.)));
        t.extend(q(v(0., 0., 0.), v(0., 1., 0.), v(1., 1., 0.), v(1., 0., 0.)));
        t.extend(q(v(0., 0., 0.), v(1., 0., 0.), v(1., 0., 1.), v(0., 0., 1.)));
        t.extend(q(v(1., 0., 0.), v(1., 1., 0.), v(1., 1., 1.), v(1., 0., 1.)));
        t.extend(q(v(1., 1., 0.), v(0., 1., 0.), v(0., 1., 1.), v(1., 1., 1.)));
        t.extend(q(v(0., 1., 0.), v(0., 0., 0.), v(0., 0., 1.), v(0., 1., 1.)));
        let mut p = Part::new(&t, &[], "top");
        p.sketches = vec![SketchPaths { id: 7, name: "Sketch".into(), paths }];
        p
    }

    fn setup(kind: OpKind, tool: Tool) -> (Cam, u64) {
        let mut c = Cam::default();
        c.stock.top = 0.0;
        c.stock.side = 0.0;
        let t = c.add_tool(tool);
        let id = c.add_op(kind, t);
        c.op_mut(id).unwrap().sketches = vec![7];
        (c, id)
    }

    fn feeds(moves: &[Move]) -> Vec<DVec3> {
        moves.iter().filter_map(|m| if let Move::Feed(p, _) = m { Some(DVec3::from(*p)) } else { None }).collect()
    }

    #[test]
    fn trace_follows_the_lines_in_passes() {
        // an L drawn as two lines, plus a closed triangle
        let l = vec![(vec![DVec3::new(5.0, 5.0, 10.0), DVec3::new(20.0, 5.0, 10.0)], false), (vec![DVec3::new(20.0, 5.0, 10.0), DVec3::new(20.0, 25.0, 10.0)], false)];
        let tri = (vec![DVec3::new(25.0, 5.0, 10.0), DVec3::new(35.0, 5.0, 10.0), DVec3::new(30.0, 15.0, 10.0), DVec3::new(25.0, 5.0, 10.0)], false);
        let part = block(l.into_iter().chain([tri]).collect());
        let (mut c, id) = setup(OpKind::Trace, Tool { kind: ToolKind::Engrave, diameter: 6.0, angle: 90.0, stepdown: 0.4, ..Default::default() });
        c.op_mut(id).unwrap().depth = 1.0;
        let paths = generate(&c, &part);
        assert!(paths[0].warnings.is_empty(), "{:?}", paths[0].warnings);
        let pts = feeds(&paths[0].moves);
        let on_line = |p: &DVec3| {
            let l = (p.y - 5.0).abs() < 1e-6 && p.x > 5.0 - 1e-6 && p.x < 20.0 + 1e-6 || (p.x - 20.0).abs() < 1e-6 && p.y > 5.0 - 1e-6 && p.y < 25.0 + 1e-6;
            let t = (p.y - 5.0).abs() < 1e-6 && p.x > 25.0 - 1e-6 && p.x < 35.0 + 1e-6
                || ((p.y - 5.0) - 2.0 * (p.x - 25.0)).abs() < 1e-6
                || ((p.y - 5.0) + 2.0 * (p.x - 35.0)).abs() < 1e-6;
            l || t
        };
        assert!(pts.iter().all(on_line), "{:?}", pts.iter().find(|p| !on_line(p)));
        // three passes down to 1 mm below the top
        let mut zs: Vec<i64> = pts.iter().map(|p| ((10.0 - p.z) * 1000.0).round() as i64).collect();
        zs.sort();
        zs.dedup();
        assert_eq!(zs, vec![333, 667, 1000]);
        // the L is one chain: it is cut without lifting between its two lines
        let rapids_down = paths[0].moves.windows(2).filter(|w| matches!(w[0], Move::Rapid(_)) && matches!(w[1], Move::Feed(..))).count();
        assert_eq!(rapids_down, 2);
    }

    #[test]
    fn vcarve_cuts_deeper_where_the_shape_is_wider() {
        // a 20 × 10 rectangle with an 8 × 4 hole: carve the ring between them
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| (vec![DVec3::new(x0, y0, 10.0), DVec3::new(x1, y0, 10.0), DVec3::new(x1, y1, 10.0), DVec3::new(x0, y1, 10.0)], true);
        let part = block(vec![sq(10.0, 10.0, 30.0, 20.0), sq(16.0, 13.0, 24.0, 17.0)]);
        let (mut c, id) = setup(OpKind::VCarve, Tool { kind: ToolKind::Engrave, diameter: 10.0, angle: 90.0, flute_length: 8.0, ..Default::default() });
        let paths = generate(&c, &part);
        assert!(paths[0].warnings.is_empty(), "{:?}", paths[0].warnings);
        let pts = feeds(&paths[0].moves);
        assert!(pts.len() > 50);
        for p in &pts {
            // inside the ring, and no deeper than the distance to its outline (90°: depth = distance)
            let out = (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(20.0 - p.y);
            let (dx, dy) = ((16.0 - p.x).max(p.x - 24.0), (13.0 - p.y).max(p.y - 17.0));
            let hole = if dx > 0.0 && dy > 0.0 { dx.hypot(dy) } else { dx.max(dy) };
            assert!(out > 0.0 && hole > 0.0, "{p}");
            assert!(10.0 - p.z <= out.min(hole) + 0.02, "{p} deeper than {}", out.min(hole));
        }
        // the deepest cut is between the hole and the short sides: 3 mm from both
        let deepest = pts.iter().map(|p| 10.0 - p.z).fold(0.0, f64::max);
        assert!((deepest - 3.0).abs() < 0.15, "{deepest}");
        // capped: a flat floor at 1 mm with a warning about its ridges
        c.op_mut(id).unwrap().depth = 1.0;
        let capped = generate(&c, &part);
        assert!((feeds(&capped[0].moves).iter().map(|p| 10.0 - p.z).fold(0.0, f64::max) - 1.0).abs() < 1e-9);
        assert!(capped[0].warnings.iter().any(|w| w.contains("ridged floor")), "{:?}", capped[0].warnings);
        // the simulator takes the carving as meant, not as gouges
        let mut s = Sim::new(&c, &part, crate::cam_paths::stock_box(&c, &part), &capped, 300);
        s.run_to(usize::MAX, usize::MAX);
        let bad: Vec<_> = s.events.iter().filter(|e| matches!(e.kind, EventKind::Gouge | EventKind::Rapid)).take(3).collect();
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn trace_beside_closed_shapes() {
        let sq = (vec![DVec3::new(10.0, 10.0, 10.0), DVec3::new(30.0, 10.0, 10.0), DVec3::new(30.0, 20.0, 10.0), DVec3::new(10.0, 20.0, 10.0)], true);
        let part = block(vec![sq]);
        let (mut c, id) = setup(OpKind::Trace, Tool { kind: ToolKind::Flat, diameter: 4.0, ..Default::default() });
        c.op_mut(id).unwrap().side = Side::Inside;
        let paths = generate(&c, &part);
        let pts = feeds(&paths[0].moves);
        assert!(!pts.is_empty());
        for p in &pts {
            let wall = (p.x - 10.0).min(30.0 - p.x).min(p.y - 10.0).min(20.0 - p.y);
            assert!(wall > 2.0 - 0.01, "{p}");
        }
        c.op_mut(id).unwrap().sketches = vec![];
        assert!(generate(&c, &part)[0].warnings.iter().any(|w| w.contains("pick the sketches")));
    }
}
