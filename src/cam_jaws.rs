//! Soft jaws: vise jaw blanks with a pocket shaped to what a setup holds (usually the part, flipped
//! for its second side). The pocket is the outline of everything below the jaw tops seen from below,
//! grown by a clearance, with reliefs in the corners the end mill can't reach, and a flat floor the part
//! sits on. The vise closes along Y; the pocket is cut with the jaws closed on a spacer `gap` apart.
//!
//! The holding setup keeps clear of the jaws like any fixture (as boxes: the jaw under the floor and
//! strips of the rim around the pocket), and checks that the stock the setup before it leaves fits.
//! A Manufacture tab with `jaws_of` set machines the jaws: its "part" is both jaw blanks as one block
//! (the gap between them filled, so nothing is cut there) with the pocket in it, in the holding
//! setup's coordinates.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glam::{DVec2, DVec3};

use crate::cam::{self, Cam, OpKind, Origin, SoftJaws, Stock, ToolKind};
use crate::cam_paths::{self, area, difference, inside, offset, rect, shapes_area, union_all, Contour, Part, PartInput, Shapes};
use crate::cam_stock::StockMap;

/// Strip width along X for the rim boxes.
const STRIP: f64 = 1.0;
/// Least floor under the pocket and wall behind it, mm.
const MIN_WALL: f64 = 3.0;

/// The jaws of a setup, in its setup coordinates.
#[derive(Clone, Debug)]
pub struct Jaws {
    /// Front and back jaw blanks (XY).
    pub rects: [(DVec2, DVec2); 2],
    /// Bottom of the blanks, the pocket floor (where the part sits) and the jaw tops.
    pub bottom: f64,
    pub floor: f64,
    pub top: f64,
    /// The pocket outline, with clearance and reliefs, across both jaws.
    pub pocket: Shapes,
    /// How the jaws bolt into the vise, and the hole centres in each jaw.
    pub mount: Option<cam::JawMount>,
    pub holes: [Vec<DVec2>; 2],
    /// Printed as one block: the gap between the rects is the slit (with its rounded ends), cut down to `web`.
    pub one_piece: bool,
    pub slit: Shapes,
    pub web: f64,
    pub warnings: Vec<String>,
}

impl Jaws {
    /// Both blanks as one block, gap included.
    pub fn block(&self) -> (DVec2, DVec2) {
        (self.rects[0].0.min(self.rects[1].0), self.rects[0].1.max(self.rects[1].1))
    }
}

static CACHE: Mutex<Option<HashMap<u64, Arc<Jaws>>>> = Mutex::new(None);

/// The soft jaws of a setup (None when it has none), worked out once per setting and part.
pub fn jaws(cam: &Cam, part: &Part) -> Option<Arc<Jaws>> {
    use std::hash::{Hash, Hasher};
    let sj = cam.soft_jaws.as_ref()?;
    let fresh = cam.follows.trim().is_empty();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(sj).unwrap_or_default().hash(&mut h);
    fresh.hash(&mut h);
    if fresh {
        let (lo, hi) = cam_paths::stock_box(cam, part);
        (lo.to_array().map(f64::to_bits), hi.to_array().map(f64::to_bits)).hash(&mut h);
    }
    // a sample of the part is enough to tell edits apart, and cheap enough to do every frame
    part.tris.len().hash(&mut h);
    for t in part.tris.iter().step_by((part.tris.len() / 1024).max(1)) {
        t.map(|p| p.to_array().map(f64::to_bits)).hash(&mut h);
    }
    let key = h.finish();
    if let Some(j) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
        return Some(j.clone());
    }
    let j = Arc::new(build(sj, cam, part));
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() > 32 {
        m.clear();
    }
    m.insert(key, j.clone());
    Some(j)
}

fn build(sj: &SoftJaws, cam: &Cam, part: &Part) -> Jaws {
    let mut warnings = vec![];
    let fresh = cam.follows.trim().is_empty();
    let grip = sj.grip.max(0.1);
    // what the jaws hold: the part as the setup before leaves it, or a fresh stock block
    let (floor, held) = if fresh {
        let (lo, hi) = cam_paths::stock_box(cam, part);
        (lo.z, rect(lo, hi))
    } else {
        (part.lo.z, from_below(part, part.lo.z + grip))
    };
    let top = floor + grip;
    let bottom = top - sj.height.max(grip + 0.1);
    // filled: posts inside holes of the part would be fragile and hard to load over
    let held = union_all(&held.iter().map(|sh| sh[0].clone()).collect::<Vec<_>>());
    let r = sj.cutter.max(0.1) / 2.0;
    let grown = offset(&held, sj.clearance.max(0.0));
    let (pocket, missed) = if sj.printed { (grown, 0.0) } else { relieve(&grown, r) };
    let (plo, phi) = bounds(&held);
    let c = (plo + phi) / 2.0;
    let (w2, g2, t) = (sj.width.max(1.0) / 2.0, sj.gap.max(0.0) / 2.0, sj.thickness.max(1.0));
    let rects = [(DVec2::new(c.x - w2, c.y - g2 - t), DVec2::new(c.x + w2, c.y - g2)), (DVec2::new(c.x - w2, c.y + g2), DVec2::new(c.x + w2, c.y + g2 + t))];
    // one printed block: the gap is a slit between its halves, rounded where it opens at the ends
    let one = sj.printed && sj.one_piece;
    let slit = if one { slit(rects[0].0.x, rects[0].1.x, rects[0].1.y, rects[1].0.y, sj.flare.max(0.0).min(t)) } else { vec![] };
    let web = if one { bottom + sj.web.max(0.0).min(top - bottom) } else { bottom };
    let mount = sj.mount.as_ref().filter(|_| !one);
    if held.is_empty() {
        warnings.push("soft jaws: nothing to hold".into());
    } else {
        if fresh {
            warnings.push("soft jaws: this setup starts from fresh stock, so the jaws hold the stock block".into());
        }
        let grips = rects.iter().all(|(a, b)| shapes_area(&crate::cam_3d::intersect(&held, &rect(a.extend(0.0), b.extend(0.0)))) > 0.01);
        if !grips {
            warnings.push(format!("soft jaws: the part is narrower than the {} mm gap between the jaws, so they can't grip it", cam::fmt_mm(sj.gap)));
        }
        let (qlo, qhi) = bounds(&pocket);
        let back = (rects[0].0.y - qlo.y).max(qhi.y - rects[1].1.y) + MIN_WALL;
        if back > 0.0 {
            warnings.push(match mount {
                // the vise sets how thick the jaws are
                Some(m) if m.fit > 0.0 => format!(
                    "soft jaws: the pocket leaves less than {MIN_WALL} mm behind it; open the gap to at least {} mm",
                    cam::fmt_mm((sj.gap + 2.0 * back).ceil())
                ),
                _ => format!("soft jaws: the pocket leaves less than {MIN_WALL} mm behind it; make the jaws at least {} mm thick", cam::fmt_mm((t + back).ceil())),
            });
        }
        if top - bottom < grip + MIN_WALL {
            warnings.push(format!("soft jaws: a {} mm deep pocket needs jaws taller than {} mm", cam::fmt_mm(grip), cam::fmt_mm(grip + MIN_WALL)));
        }
        if missed > 0.5 {
            warnings.push(format!("soft jaws: parts of the pocket ({} mm²) are too narrow for the {} mm cutter", cam::fmt_mm(missed.round()), cam::fmt_mm(sj.cutter)));
        }
    }
    let mut holes = [vec![], vec![]];
    if let Some(m) = mount {
        for (k, (a, b)) in rects.iter().enumerate() {
            // inset from the grip face: the back of the front jaw, the front of the back one
            let y = if k == 0 { b.y - m.inset } else { a.y + m.inset };
            holes[k] = m.holes.iter().map(|x| DVec2::new(c.x + x, y)).collect();
        }
        mount_warnings(sj, m, &holes, &pocket, held.is_empty(), (bottom, floor, top), &mut warnings);
    }
    Jaws { rects, bottom, floor, top, pocket, mount: mount.cloned(), holes, one_piece: one, slit, web, warnings }
}

/// Whether jaws bolted on with `m` fit the vise and keep clear of their screws.
#[allow(clippy::too_many_arguments)]
fn mount_warnings(sj: &SoftJaws, m: &cam::JawMount, holes: &[Vec<DVec2>; 2], pocket: &Shapes, empty: bool, z: (f64, f64, f64), w: &mut Vec<String>) {
    let (bottom, floor, top) = z;
    if m.fit > 0.0 && (sj.thickness - m.fit).abs() > 0.01 {
        w.push(format!("soft jaws: the {} takes jaws {} mm thick, not {} mm", m.name, cam::fmt_mm(m.fit), cam::fmt_mm(sj.thickness)));
    }
    if m.holes.iter().any(|x| x.abs() + m.cbore / 2.0 > sj.width / 2.0) {
        w.push(format!("soft jaws: the screw holes don't fit in jaws {} mm long", cam::fmt_mm(sj.width)));
    }
    if m.seat > 0.0 && top - bottom < m.seat + 1.0 {
        w.push(format!("soft jaws: jaws {} mm tall are too short for the screws' counterbores", cam::fmt_mm(top - bottom)));
    }
    if sj.gap < 2.0 * m.lip - 1e-6 {
        w.push(format!("soft jaws: the vise's lips in front of the jaws keep them at least {} mm apart", cam::fmt_mm(2.0 * m.lip)));
    }
    if empty {
        return;
    }
    if floor < bottom + m.lip_height - 1e-6 {
        w.push(format!(
            "soft jaws: the part sits {} mm below the top of the vise's lip in front of the jaws; make the jaws taller",
            cam::fmt_mm(((bottom + m.lip_height - floor) * 10.0).ceil() / 10.0)
        ));
    }
    let hit = holes.iter().flatten().filter(|h| shapes_area(&crate::cam_3d::intersect(pocket, &vec![vec![circle(**h, m.cbore / 2.0)]])) > 0.01).count();
    // breaking into a counterbore is fine above the screw head
    let heads = bottom + m.seat + m.head;
    if hit > 0 && floor < heads + 0.5 {
        w.push(format!(
            "soft jaws: the pocket reaches down to the screw heads in {hit} of the counterbores; make the jaws {} mm taller or open the gap",
            cam::fmt_mm((heads + 0.5 - floor).ceil())
        ));
    }
}

/// The slit between the halves of a printed block, from y0 to y1 along its length x0..x1, with rounds of
/// radius `r` where it opens at the ends.
fn slit(x0: f64, x1: f64, y0: f64, y1: f64, r: f64) -> Shapes {
    let mut cs = vec![vec![[x0 - 1.0, y0], [x1 + 1.0, y0], [x1 + 1.0, y1], [x0 - 1.0, y1]]];
    if r > 1e-3 {
        let n = 12;
        for (xe, sx) in [(x0, 1.0), (x1, -1.0)] {
            for (ye, sy) in [(y0, -1.0), (y1, 1.0)] {
                // the corner of the half (xe, ye) less a quarter disc round (xe + sx r, ye + sy r)
                let cen = DVec2::new(xe + sx * r, ye + sy * r);
                let mut c = vec![[xe - sx, ye], [xe - sx, ye + sy * r]];
                for k in 0..=n {
                    let t = std::f64::consts::FRAC_PI_2 * k as f64 / n as f64;
                    let p = cen + DVec2::new(-sx * t.cos(), -sy * t.sin()) * r;
                    c.push([p.x, p.y]);
                }
                if area(&c) < 0.0 {
                    c.reverse();
                }
                cs.push(c);
            }
        }
    }
    union_all(&cs)
}

/// A circle as a polygon (counter-clockwise).
fn circle(c: DVec2, r: f64) -> Contour {
    let n = 32;
    (0..n).map(|k| {
        let t = std::f64::consts::TAU * k as f64 / n as f64;
        [c.x + r * t.cos(), c.y + r * t.sin()]
    }).collect()
}

/// Outline of the part below `z`, seen from below (its downward faces clipped to z and projected).
fn from_below(part: &Part, z: f64) -> Shapes {
    let mut polys: Vec<Contour> = vec![];
    for t in &part.tris {
        if (t[1] - t[0]).cross(t[2] - t[0]).z >= -1e-12 || t[0].z.min(t[1].z).min(t[2].z) >= z {
            continue;
        }
        let mut pts: Vec<DVec3> = vec![];
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            if a.z <= z {
                pts.push(a);
            }
            if (a.z <= z) != (b.z <= z) {
                pts.push(a + (b - a) * ((z - a.z) / (b.z - a.z)));
            }
        }
        let c: Contour = pts.iter().map(|p| [p.x, p.y]).collect();
        if c.len() >= 3 && area(&c).abs() > 1e-9 {
            polys.push(c);
        }
    }
    union_all(&polys)
}

fn bounds(s: &Shapes) -> (DVec2, DVec2) {
    let mut lo = DVec2::splat(f64::INFINITY);
    let mut hi = DVec2::splat(f64::NEG_INFINITY);
    for p in s.iter().flat_map(|sh| sh.iter()).flat_map(|c| c.iter()) {
        lo = lo.min(DVec2::from(*p));
        hi = hi.max(DVec2::from(*p));
    }
    if !lo.x.is_finite() {
        return (DVec2::ZERO, DVec2::ZERO);
    }
    (lo, hi)
}

/// Add reliefs where an end mill of radius `r` can't reach into the outline (its sharp outside corners):
/// for each spot it misses, a slot from where the tool can go to just past the farthest missed point.
/// Returns the relieved outline and the area still out of reach.
fn relieve(s: &Shapes, r: f64) -> (Shapes, f64) {
    let mut cur = s.clone();
    for _ in 0..3 {
        let reach = offset(&cur, -r);
        if reach.is_empty() {
            return (cur.clone(), shapes_area(&cur));
        }
        let missed: Shapes = difference(&cur, &offset(&reach, r)).into_iter().filter(|sh| area(&sh[0]).abs() > 1e-3).collect();
        if missed.is_empty() {
            return (cur, 0.0);
        }
        let segs: Vec<(DVec2, DVec2)> =
            reach.iter().flat_map(|sh| sh.iter()).flat_map(|c| (0..c.len()).map(move |i| (DVec2::from(c[i]), DVec2::from(c[(i + 1) % c.len()])))).collect();
        let mut slots: Vec<Contour> = vec![];
        for sh in &missed {
            // the missed point farthest from where the tool's centre can go, and the nearest place it can
            let far = sh[0]
                .iter()
                .map(|v| {
                    let q = DVec2::from(*v);
                    let e = nearest(&segs, q);
                    (q, e, q.distance(e))
                })
                .max_by(|a, b| a.2.total_cmp(&b.2));
            let Some((q, e, d)) = far else { continue };
            if d < 1e-6 {
                continue;
            }
            let c = q + (e - q) / d * (r * 0.9);
            slots.push(capsule(e, c, r + 0.05));
        }
        if slots.is_empty() {
            return (cur, missed.iter().map(|sh| area(&sh[0]).abs()).sum());
        }
        let mut all: Vec<Contour> = cur.iter().flat_map(|sh| sh.iter().cloned()).collect();
        all.extend(slots);
        cur = union_all(&all);
    }
    let reach = offset(&cur, -r);
    let left = shapes_area(&difference(&cur, &offset(&reach, r)));
    (cur, left)
}

fn nearest(segs: &[(DVec2, DVec2)], q: DVec2) -> DVec2 {
    let mut best = (f64::INFINITY, q);
    for (a, b) in segs {
        let ab = *b - *a;
        let t = if ab.length_squared() < 1e-18 { 0.0 } else { ((q - *a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
        let p = *a + ab * t;
        let d = p.distance_squared(q);
        if d < best.0 {
            best = (d, p);
        }
    }
    best.1
}

/// A slot of radius `r` from `a` to `b` (counter-clockwise).
fn capsule(a: DVec2, b: DVec2, r: f64) -> Contour {
    let n = 24;
    let d = b - a;
    let ang = if d.length() < 1e-9 { 0.0 } else { d.y.atan2(d.x) };
    let mut c = vec![];
    // half circle round b, then round a
    for k in 0..=n {
        let t = ang - std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * k as f64 / n as f64;
        c.push([b.x + r * t.cos(), b.y + r * t.sin()]);
    }
    for k in 0..=n {
        let t = ang + std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * k as f64 / n as f64;
        c.push([a.x + r * t.cos(), a.y + r * t.sin()]);
    }
    c
}

/// The jaws as boxes for the toolpaths to lift over and the simulation to check: each blank under the
/// pocket floor, and the rim around the pocket in strips along X.
pub fn boxes(j: &Jaws) -> Vec<(String, DVec3, DVec3)> {
    let mut out = vec![];
    let (plo, phi) = bounds(&j.pocket);
    for (k, (a, b)) in j.rects.iter().enumerate() {
        let name = if k == 0 { "Soft jaw (front)" } else { "Soft jaw (back)" };
        out.push((name.to_string(), DVec3::new(a.x, a.y, j.bottom), DVec3::new(b.x, b.y, j.floor)));
        // rim pieces as [x0, x1, y0, y1], merged along X where they line up
        let mut done: Vec<[f64; 4]> = vec![];
        let mut open: Vec<[f64; 4]> = vec![];
        let mut x = a.x;
        while x < b.x - 1e-9 {
            // away from the pocket in one piece
            let x1 = if x < plo.x {
                plo.x.min(b.x)
            } else if x >= phi.x {
                b.x
            } else {
                (x + STRIP).min(b.x).min(phi.x.max(x + 1e-3))
            };
            let mut pieces: Vec<[f64; 4]> = vec![];
            rim(x, x1, a.y, b.y, &j.pocket, &mut pieces);
            let mut next = vec![];
            for p in pieces {
                if let Some(i) = open.iter().position(|o| (o[2] - p[2]).abs() < 1e-3 && (o[3] - p[3]).abs() < 1e-3 && (o[1] - p[0]).abs() < 1e-9) {
                    let mut o = open.remove(i);
                    o[1] = p[1];
                    next.push(o);
                } else {
                    next.push(p);
                }
            }
            done.append(&mut open);
            open = next;
            x = x1;
        }
        done.append(&mut open);
        for p in done {
            out.push((name.to_string(), DVec3::new(p[0], p[2], j.floor), DVec3::new(p[1], p[3], j.top)));
        }
        // the vise steel the jaw sits in: its lip toward the part and the vise jaw behind
        if let Some(m) = &j.mount {
            let (grip, back, s) = if k == 0 { (b.y, a.y, 1.0) } else { (a.y, b.y, -1.0) };
            let (side, z) = (if k == 0 { "front" } else { "back" }, j.bottom);
            if m.lip > 0.0 && m.lip_height > 0.0 {
                let (y0, y1) = (grip, grip + s * m.lip);
                out.push((format!("Vise lip ({side})"), DVec3::new(a.x, y0.min(y1), z), DVec3::new(b.x, y0.max(y1), z + m.lip_height)));
            }
            if m.back > 0.0 && m.back_height > 0.0 {
                let (y0, y1) = (back, back - s * m.back);
                out.push((format!("Vise jaw ({side})"), DVec3::new(a.x, y0.min(y1), z), DVec3::new(b.x, y0.max(y1), z + m.back_height)));
            }
        }
    }
    out
}

/// The rim of a jaw between x0 and x1 as boxes [x0, x1, y0, y1] covering it, narrower strips where a
/// box would reach well into the pocket (across its ends).
fn rim(x0: f64, x1: f64, y0: f64, y1: f64, pocket: &Shapes, out: &mut Vec<[f64; 4]>) {
    let strip = rect(DVec3::new(x0, y0, 0.0), DVec3::new(x1, y1, 0.0));
    let rest = difference(&strip, pocket);
    let mut pieces = vec![];
    for sh in rest {
        let fill = area(&sh[0]).abs();
        let (lo, hi) = bounds(&vec![sh]);
        if (x1 - x0) * (hi.y - lo.y) > fill * 1.05 + 0.02 && x1 - x0 > 0.1 {
            let m = (x0 + x1) / 2.0;
            rim(x0, m, y0, y1, pocket, out);
            rim(m, x1, y0, y1, pocket, out);
            return;
        }
        pieces.push([x0, x1, lo.y, hi.y]);
    }
    out.extend(pieces);
}

/// Triangles of a jaw blank (`rect`) with the pocket and any screw holes in it: slabs between the heights
/// where its section changes, each with its walls, and flat faces where one slab's section steps to the next.
fn solid(rect_xy: (DVec2, DVec2), pocket: &Shapes, holes: &[DVec2], mount: Option<&cam::JawMount>, slit: (&Shapes, f64), bottom: f64, floor: f64, top: f64) -> Vec<[DVec3; 3]> {
    let (a, b) = rect_xy;
    let r = rect(a.extend(0.0), b.extend(0.0));
    let circles = |d: f64| -> Shapes { union_all(&holes.iter().map(|h| circle(*h, d / 2.0)).collect::<Vec<_>>()) };
    let (drill, cbore, seat) = match mount {
        Some(m) if !holes.is_empty() => (circles(m.drill), circles(m.cbore), bottom + m.seat),
        _ => (vec![], vec![], bottom),
    };
    let section = |z: f64| {
        let mut s = difference(&r, &drill);
        if z > floor {
            s = difference(&s, pocket);
        }
        if z > seat && !cbore.is_empty() {
            s = difference(&s, &cbore);
        }
        if z > slit.1 && !slit.0.is_empty() {
            s = difference(&s, slit.0);
        }
        s
    };
    let mut levels = vec![bottom, top];
    for z in [floor, seat, slit.1] {
        if z > bottom + 1e-6 && z < top - 1e-6 {
            levels.push(z);
        }
    }
    levels.sort_by(f64::total_cmp);
    levels.dedup_by(|x, y| (*x - *y).abs() < 1e-6);
    let slabs: Vec<Shapes> = levels.windows(2).map(|w| section((w[0] + w[1]) / 2.0)).collect();
    let mut tris = vec![];
    for (k, s) in slabs.iter().enumerate() {
        // walls facing out of the section
        for sh in s {
            let ccw = area(&sh[0]) > 0.0;
            for c in sh {
                for i in 0..c.len() {
                    let (p, q) = (DVec2::from(c[i]), DVec2::from(c[(i + 1) % c.len()]));
                    let (p, q) = if ccw { (q, p) } else { (p, q) };
                    quad(&mut tris, p, q, levels[k], levels[k + 1]);
                }
            }
        }
        if k == 0 {
            fill(&mut tris, s, levels[0], false);
        }
        if k + 1 == slabs.len() {
            fill(&mut tris, s, levels[k + 1], true);
        } else {
            fill(&mut tris, &difference(s, &slabs[k + 1]), levels[k + 1], true);
            fill(&mut tris, &difference(&slabs[k + 1], s), levels[k + 1], false);
        }
    }
    tris
}

/// A vertical wall from `p` to `q` between two heights, facing left of p→q.
fn quad(tris: &mut Vec<[DVec3; 3]>, p: DVec2, q: DVec2, z0: f64, z1: f64) {
    if z1 - z0 < 1e-9 || p.distance(q) < 1e-9 {
        return;
    }
    let (p0, p1, q0, q1) = (p.extend(z0), p.extend(z1), q.extend(z0), q.extend(z1));
    tris.push([p0, p1, q0]);
    tris.push([q0, p1, q1]);
}

/// Flat faces at height z, facing up or down.
fn fill(tris: &mut Vec<[DVec3; 3]>, s: &Shapes, z: f64, up: bool) {
    for sh in s {
        let mut coords: Vec<f64> = vec![];
        let mut holes = vec![];
        for (i, c) in sh.iter().enumerate() {
            if i > 0 {
                holes.push(coords.len() / 2);
            }
            for p in c {
                coords.extend(p);
            }
        }
        let Ok(idx) = earcutr::earcut(&coords, &holes, 2) else { continue };
        let pt = |i: usize| DVec3::new(coords[2 * i], coords[2 * i + 1], z);
        for t in idx.chunks(3) {
            let (p, q, r) = (pt(t[0]), pt(t[1]), pt(t[2]));
            let n = (q - p).cross(r - p).z;
            if n.abs() < 1e-12 {
                continue;
            }
            tris.push(if (n > 0.0) == up { [p, q, r] } else { [p, r, q] });
        }
    }
}

/// Both jaws as they sit in the vise holding the part (setup coordinates).
pub fn jaw_tris(j: &Jaws) -> Vec<[DVec3; 3]> {
    if j.one_piece {
        return solid(j.block(), &j.pocket, &[], None, (&j.slit, j.web), j.bottom, j.floor, j.top);
    }
    j.rects.iter().zip(&j.holes).flat_map(|(r, h)| solid(*r, &j.pocket, h, j.mount.as_ref(), (&vec![], j.bottom), j.bottom, j.floor, j.top)).collect()
}

/// What the jaws' own setup machines: both blanks as one block (the spacer gap filled) with the pocket.
/// The screw holes are left out: they are in the blanks before they go in the vise.
pub fn block_tris(j: &Jaws) -> Vec<[DVec3; 3]> {
    solid(j.block(), &j.pocket, &[], None, (&vec![], j.bottom), j.bottom, j.floor, j.top)
}

/// The setup a jaw-machining tab makes jaws for.
pub fn held_setup(cams: &[Cam], ci: usize) -> Result<Option<usize>, String> {
    let name = cams[ci].jaws_of.trim();
    if name.is_empty() {
        return Ok(None);
    }
    match cams.iter().position(|c| c.name == name) {
        Some(i) if i == ci => Err("a setup can't machine its own soft jaws".into()),
        Some(i) if cams[i].soft_jaws.is_none() => Err(format!("\"{name}\" has no soft jaws")),
        Some(i) => Ok(Some(i)),
        None => Err(format!("{} machines the soft jaws of \"{name}\", but there is no Manufacture tab of that name", cams[ci].name)),
    }
}

/// What a setup machines: the Part Studio's parts, or for a jaw-machining tab the jaws of the setup it
/// names (in that setup's coordinates, which are this one's).
pub fn input_for(cams: &[Cam], ci: usize, input: PartInput) -> Result<PartInput, String> {
    let Some(h) = held_setup(cams, ci)? else { return Ok(input) };
    let held = &cams[h];
    let part = Part::from_input(&input, held);
    let Some(j) = jaws(held, &part) else { return Ok(input) };
    let back = cam_paths::setup_matrix(&cams[ci]).transpose();
    let tris = block_tris(&j).into_iter().map(|t| t.map(|p| back * p)).collect();
    Ok(PartInput { tris, circles: vec![], sketches: vec![] })
}

/// Warnings for a setup that holds the part in soft jaws: the jaws' own, and whether the stock the
/// setups before it leave (`stock`, setup coordinates) fits the pocket.
pub fn fit_warnings(cam: &Cam, part: &Part, stock: Option<&StockMap>) -> Vec<String> {
    let Some(j) = jaws(cam, part) else { return vec![] };
    let mut w = j.warnings.clone();
    let Some(m) = stock else { return w };
    let o = cam_paths::work_origin(cam, part);
    let segs: Vec<(DVec2, DVec2)> =
        j.pocket.iter().flat_map(|sh| sh.iter()).flat_map(|c| (0..c.len()).map(move |i| (DVec2::from(c[i]), DVec2::from(c[(i + 1) % c.len()])))).collect();
    // material inside a jaw blank below its top, outside the pocket
    let mut worst: Option<(f64, DVec2)> = None;
    let mut low = f64::INFINITY;
    for j0 in 0..m.ny {
        let y = m.y0 + j0 as f64 * m.cell;
        for i0 in 0..m.nx {
            let x = m.x0 + i0 as f64 * m.cell;
            let Some((lo, hi)) = m.column(x, y) else { continue };
            if hi <= lo {
                continue;
            }
            low = low.min(lo as f64);
            if lo as f64 >= j.top - 0.01 || !j.rects.iter().any(|(a, b)| x > a.x && x < b.x && y > a.y && y < b.y) || inside(&j.pocket, [x, y]) {
                continue;
            }
            let p = DVec2::new(x, y);
            let d = nearest(&segs, p).distance(p);
            if worst.map(|w| d > w.0).unwrap_or(true) {
                worst = Some((d, p));
            }
        }
    }
    // a cell or so is the simulation's resolution
    if let Some((d, p)) = worst.filter(|w| w.0 > m.cell * 1.5) {
        w.push(format!(
            "soft jaws: the stock {} leaves sticks out of the pocket by up to {} mm, near X{} Y{}: machine that side below the grip depth there, or grip less",
            cam.follows.trim(),
            cam::fmt_mm((d * 10.0).ceil() / 10.0),
            cam::fmt_mm((p.x - o.x).round()),
            cam::fmt_mm((p.y - o.y).round())
        ));
    }
    if low < j.floor - 0.05 && !cam.follows.trim().is_empty() {
        w.push(format!(
            "soft jaws: the stock {} leaves reaches {} mm below the part, so it would sit that much high in the jaws: face that side down to the part",
            cam.follows.trim(),
            cam::fmt_mm(((j.floor - low) * 10.0).ceil() / 10.0)
        ));
    }
    w
}

/// A new Manufacture tab that machines the soft jaws of setup `held`: the blanks closed on a spacer in the
/// vise, work zero on top in the middle, with an end mill of the jaws' cutter size skimming the tops,
/// roughing the pocket and finishing its walls.
pub fn jaw_setup(held: &Cam) -> Cam {
    let sj = held.soft_jaws.clone().unwrap_or_default();
    let mut c = Cam {
        name: format!("{} jaws", held.name),
        studio: held.studio,
        orient: "top".into(),
        jaws_of: held.name.clone(),
        origin: Origin::StockTopCenter,
        stock: Stock { side: 0.0, top: 0.5, bottom: 0.0, size: None, material: sj.material.clone(), carried: None },
        clearance: held.clearance,
        retract: held.retract,
        rapid_rate: held.rapid_rate,
        max_rpm: held.max_rpm,
        min_rpm: held.min_rpm,
        post: held.post.clone(),
        program: held.program + 1,
        machine: held.machine.clone(),
        coolant: held.coolant,
        arcs: held.arcs,
        probe: held.probe.clone(),
        post_text: held.post_text.clone(),
        ..Default::default()
    };
    let mut t = cam::presets()
        .into_iter()
        .filter(|t| t.kind == ToolKind::Flat && t.diameter <= sj.cutter + 1e-6)
        .max_by(|a, b| a.diameter.total_cmp(&b.diameter))
        .unwrap_or_else(|| cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat).unwrap_or_default());
    cam::suggest(&mut t, &sj.material, c.max_rpm, c.min_rpm);
    let tool = c.add_tool(t);
    let face = c.add_op(OpKind::Face, tool);
    c.op_mut(face).unwrap().name = "Skim jaw tops".into();
    let rough = c.add_op(OpKind::Clear, tool);
    if let Some(o) = c.op_mut(rough) {
        o.name = "Rough pocket".into();
        o.rest = false;
        o.leave_floor = 0.0;
    }
    // the walls to size (when the part is longer than the jaws the pocket is open: finish it with a contour
    // on both sides or a clearing pass instead)
    let finish = c.add_op(OpKind::Contour, tool);
    if let Some(o) = c.op_mut(finish) {
        o.name = "Finish pocket walls".into();
        o.side = cam::Side::Inside;
        o.leave = 0.0;
    }
    c
}

/// Where the holding setup's work zero is from the jaw tab's (both in the same coordinates).
pub fn zero_offset(jaw_cam: &Cam, jaw_part: &Part, held: &Cam, held_part: &Part) -> DVec3 {
    cam_paths::work_origin(held, held_part) - cam_paths::work_origin(jaw_cam, jaw_part)
}

/// The jaws as a Part Studio for 3D printing: each blank standing on its base with the pocket up, the two
/// side by side at least 10 mm apart (or the one block), built from extrudes (a base up to the pocket floor and the rim around
/// the pocket on top of it) so they are solids that export to STEP.
pub fn print_doc(j: &Jaws) -> crate::doc::Document {
    use crate::doc::{BoolOp, Document, EndType, Op, OriginPlane, Ref};
    use crate::sketch::{Geom, Sketch};
    let mut doc = Document::default();
    let (blo, _) = j.block();
    let gap = j.rects[1].0.y - j.rects[0].1.y;
    let h = j.top - j.bottom;
    let base = j.floor - j.bottom;
    let extrude = |doc: &mut Document, name: &str, shapes: &Shapes, shift: DVec2, depth: f64, op: BoolOp| {
        let mut sk = Sketch::default();
        let mut regions = vec![];
        for sh in shapes {
            // straight lines between points (a closed Poly would become a smooth spline)
            for c in sh {
                let ids: Vec<_> = c.iter().map(|p| sk.add_point(DVec2::from(*p) + shift)).collect();
                for i in 0..ids.len() {
                    sk.add_curve(Geom::Line { a: ids[i], b: ids[(i + 1) % ids.len()] }, false);
                }
            }
            if let Some(p) = interior(sh) {
                regions.push(p + shift);
            }
        }
        let sid = doc.insert(doc.next_name("sketch"), Op::Sketch { plane: Ref::OriginPlane { plane: OriginPlane::Top }, sketch: sk, frame: crate::sketch::Frame::TOP });
        let profiles = regions.iter().map(|p| Ref::Region { sketch: sid, point: [p.x, p.y] }).collect();
        let s = |v: f64| crate::cam::fmt_mm((v * 1000.0).round() / 1000.0);
        doc.insert(
            name.to_string(),
            Op::Extrude { profiles, end: EndType::Blind, depth: s(depth), opposite: false, second: false, depth2: s(depth), up_to: None, op, draft: "0".into() },
        );
    };
    let pieces: Vec<(DVec2, DVec2)> = if j.one_piece { vec![j.block()] } else { j.rects.to_vec() };
    for (k, (a, b)) in pieces.iter().enumerate() {
        let shift = -blo + DVec2::new(0.0, if k == 1 { (10.0 - gap).max(0.0) } else { 0.0 });
        let r = rect(a.extend(0.0), b.extend(0.0));
        let side = if j.one_piece { "block" } else if k == 0 { "front" } else { "back" };
        let rim = difference(&r, &j.pocket);
        if j.one_piece {
            // the slit's web, the halves on it up to the pocket floor, then the rim round the pocket
            // (one extrude per half: each makes or joins one solid)
            let web = j.web - j.bottom;
            if web > 1e-3 {
                extrude(&mut doc, "Block web", &r, shift, web, BoolOp::New);
            }
            for (half, (a, b)) in ["front", "back"].iter().zip(&j.rects) {
                let mut op = if web > 1e-3 { BoolOp::Add } else { BoolOp::New };
                let sh = difference(&rect(a.extend(0.0), b.extend(0.0)), &j.slit);
                if base > web + 1e-3 {
                    extrude(&mut doc, &format!("Block half ({half})"), &sh, shift, base, op);
                    op = BoolOp::Add;
                }
                extrude(&mut doc, &format!("Block rim ({half})"), &difference(&sh, &j.pocket), shift, h, op);
            }
            continue;
        }
        if base > 1e-3 {
            extrude(&mut doc, &format!("Jaw base ({side})"), &r, shift, base, BoolOp::New);
            extrude(&mut doc, &format!("Jaw rim ({side})"), &rim, shift, h, BoolOp::Add);
        } else {
            // the pocket goes right through
            extrude(&mut doc, &format!("Jaw ({side})"), &rim, shift, h, BoolOp::New);
        }
        if let Some(m) = j.mount.as_ref().filter(|_| !j.holes[k].is_empty()) {
            // through holes up from the base, counterbores down from the top
            let circles = |doc: &mut Document, plane: Ref, d: f64| {
                let mut sk = Sketch::default();
                for c in &j.holes[k] {
                    let id = sk.add_point(*c + shift);
                    sk.add_curve(Geom::Circle { c: id, r: d / 2.0 }, false);
                }
                let sid = doc.insert(doc.next_name("sketch"), Op::Sketch { plane, sketch: sk, frame: crate::sketch::Frame::TOP });
                j.holes[k].iter().map(|c| Ref::Region { sketch: sid, point: [c.x + shift.x, c.y + shift.y] }).collect::<Vec<_>>()
            };
            let s = |v: f64| crate::cam::fmt_mm((v * 1000.0).round() / 1000.0);
            let profiles = circles(&mut doc, Ref::OriginPlane { plane: OriginPlane::Top }, m.drill);
            doc.insert(
                format!("Screw holes ({side})"),
                Op::Extrude { profiles, end: EndType::Blind, depth: s(h), opposite: false, second: false, depth2: s(h), up_to: None, op: BoolOp::Remove, draft: "0".into() },
            );
            if m.cbore > m.drill && m.seat < h {
                let pid = doc.insert(
                    doc.next_name("plane"),
                    Op::Plane { base: Some(Ref::OriginPlane { plane: OriginPlane::Top }), offset: s(h), mode: "offset".into(), refs: vec![], angle: "0".into(), flip: false },
                );
                let profiles = circles(&mut doc, Ref::FeaturePlane { feature: pid }, m.cbore);
                doc.insert(
                    format!("Counterbores ({side})"),
                    Op::Extrude { profiles, end: EndType::Blind, depth: s(h - m.seat), opposite: true, second: false, depth2: s(h), up_to: None, op: BoolOp::Remove, draft: "0".into() },
                );
            }
        }
    }
    doc
}

/// A point inside a shape (the middle of its biggest triangle).
fn interior(sh: &[Contour]) -> Option<DVec2> {
    let mut tris = vec![];
    fill(&mut tris, &vec![sh.to_vec()], 0.0, true);
    tris.iter().max_by(|a, b| tri_area(a).total_cmp(&tri_area(b))).map(|t| ((t[0] + t[1] + t[2]) / 3.0).truncate())
}

fn tri_area(t: &[DVec3; 3]) -> f64 {
    (t[1] - t[0]).cross(t[2] - t[0]).length()
}

/// Triangles as a mesh body for the renderers.
pub fn body_mesh(name: &str, tris: &[[DVec3; 3]]) -> crate::kernel::BodyMesh {
    let mut m = crate::kernel::BodyMesh { name: name.into(), ..Default::default() };
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for t in tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        for p in t {
            m.indices.push((m.positions.len() / 3) as u32);
            m.positions.extend([p.x as f32, p.y as f32, p.z as f32]);
            m.normals.extend([n.x as f32, n.y as f32, n.z as f32]);
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
    }
    if lo.x.is_finite() {
        m.bbox = [lo.to_array(), hi.to_array()];
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam_paths::{generate_from, Ctx};

    /// A 60 × 30 × 10 block with square corners: its outline is the rectangle.
    fn block(lo: DVec3, hi: DVec3) -> Vec<[DVec3; 3]> {
        let c = |k: usize| DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]);
        let f = [[0, 2, 3], [0, 3, 1], [4, 5, 7], [4, 7, 6], [0, 1, 5], [0, 5, 4], [2, 6, 7], [2, 7, 3], [0, 4, 6], [0, 6, 2], [1, 3, 7], [1, 7, 5]];
        f.iter().map(|t| [c(t[0]), c(t[1]), c(t[2])]).collect()
    }

    fn held_cam() -> Cam {
        let mut c = Cam { name: "OP2".into(), follows: "OP1".into(), soft_jaws: Some(SoftJaws { width: 100.0, ..Default::default() }), ..Default::default() };
        c.stock.carried = Some([[-2.0, -2.0, 0.0], [62.0, 32.0, 11.0]]);
        c
    }

    #[test]
    fn pocket_follows_the_part_with_reliefs() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let c = held_cam();
        let j = jaws(&c, &part).unwrap();
        assert!(j.warnings.is_empty(), "{:?}", j.warnings);
        assert!((j.floor - 0.0).abs() < 1e-9 && (j.top - 6.0).abs() < 1e-9 && (j.bottom + 34.0).abs() < 1e-9);
        // the jaws meet the part's long sides, gap centred on it
        assert!((j.rects[0].1.y - 13.0).abs() < 1e-9 && (j.rects[1].0.y - 17.0).abs() < 1e-9);
        // pocket: the outline plus clearance, and every corner relieved so a 6 mm end mill reaches it
        assert!(inside(&j.pocket, [60.04, 15.0]) && !inside(&j.pocket, [60.2, 15.0]));
        for p in [[0.0f64, 0.0f64], [60.0, 0.0], [60.0, 30.0], [0.0, 30.0]] {
            assert!(inside(&j.pocket, [p[0] + (p[0] - 30.0).signum() * 0.04, p[1] + (p[1] - 15.0).signum() * 0.04]), "corner {p:?}");
        }
        let reach = offset(&offset(&j.pocket, -3.0), 3.0);
        for p in [[-0.04, -0.04], [60.04, 30.04]] {
            assert!(inside(&reach, p), "the cutter reaches {p:?}");
        }
    }

    #[test]
    fn jaw_block_machines_to_the_pocket() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let held = held_cam();
        let j = jaws(&held, &part).unwrap();
        // the jaws' setup: its part is the block with the pocket
        let mut cams = vec![held.clone(), jaw_setup(&held)];
        cams[0].name = "OP2".into();
        let input = input_for(&cams, 1, PartInput { tris: part.tris.clone(), ..Default::default() }).unwrap();
        let jp = Part::from_input(&input, &cams[1]);
        assert!((jp.hi.z - j.top).abs() < 1e-9 && (jp.lo.z - j.bottom).abs() < 1e-9);
        let ctx = Ctx::new(&cams[1], &jp);
        let sh = ctx.geo.shadow(3.0);
        assert!(!inside(&sh, [30.0, 15.0]) && inside(&sh, [30.0, 1.0 - 12.0]) && inside(&sh, [-10.0, 15.0]));
        let paths = generate_from(&cams[1], &jp, None);
        for tp in &paths {
            assert!(!tp.moves.is_empty(), "{:?}", tp.warnings);
        }
        // the rough pass reaches the pocket floor, the finish pass the walls
        let floor = |k: usize| paths[k].moves.iter().any(|m| matches!(m, cam_paths::Move::Feed(p, _) if (p[2] - j.floor).abs() < 1e-6));
        assert!(floor(1) && floor(2));
        let feeds: Vec<DVec3> = paths[2].moves.iter().filter_map(|m| if let cam_paths::Move::Feed(p, _) = m { Some(DVec3::from(*p)) } else { None }).collect();
        let r = cams[1].tools[0].radius();
        // (the straight end wall runs between two corners of the loop)
        assert!(feeds.iter().filter(|p| (p.x - (60.05 - r)).abs() < 0.02 && (p.y - 15.0).abs() < 14.0).count() >= 2, "finishes the end wall");
    }

    #[test]
    fn holding_setup_keeps_clear_of_the_jaws() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let c = held_cam();
        let o = cam_paths::work_origin(&c, &part);
        let bx = cam_paths::fixture_boxes(&c, &part, o);
        assert!(bx.iter().any(|b| b.0.contains("Soft jaw")));
        // no box reaches into the part above the floor
        for (_, lo, hi) in &bx {
            if hi.z > 0.01 {
                let into = lo.x < 59.9 && hi.x > 0.1 && lo.y < 29.9 && hi.y > 0.1;
                assert!(!into, "{lo:?} {hi:?}");
            }
        }
        // the stock fits; a stub of stock outside the pocket doesn't
        let mut m = StockMap::block(DVec3::new(-1.0, -1.0, 0.0), DVec3::new(61.0, 31.0, 11.0), 0.2);
        for k in 0..m.lo.len() {
            let (x, y) = (m.x0 + (k % m.nx) as f64 * m.cell, m.y0 + (k / m.nx) as f64 * m.cell);
            if x < 0.0 || x > 60.0 || y < 0.0 || y > 30.0 {
                m.lo[k] = 7.0;
            }
        }
        assert_eq!(fit_warnings(&c, &part, Some(&m)).len(), 0, "{:?}", fit_warnings(&c, &part, Some(&m)));
        for k in 0..m.lo.len() {
            m.lo[k] = 0.0;
        }
        let w = fit_warnings(&c, &part, Some(&m));
        assert!(w.iter().any(|w| w.contains("sticks out")), "{w:?}");
    }

    #[test]
    fn narrow_part_and_thin_jaws_warn() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 3.0, 10.0)), &[], "top");
        let mut c = held_cam();
        let j = jaws(&c, &part).unwrap();
        assert!(j.warnings.iter().any(|w| w.contains("can't grip")), "{:?}", j.warnings);
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 60.0, 10.0)), &[], "top");
        c.soft_jaws.as_mut().unwrap().thickness = 20.0;
        let j = jaws(&c, &part).unwrap();
        assert!(j.warnings.iter().any(|w| w.contains("thick")), "{:?}", j.warnings);
    }

    #[test]
    fn printed_jaws_have_sharp_corners_and_a_part_studio() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let mut c = held_cam();
        c.soft_jaws.as_mut().unwrap().printed = true;
        c.soft_jaws.as_mut().unwrap().clearance = 0.15;
        let j = jaws(&c, &part).unwrap();
        // no relief slots: just past a corner is still jaw
        assert!(inside(&j.pocket, [60.1, 15.0]) && !inside(&j.pocket, [60.5, 30.5]));
        let doc = print_doc(&j);
        // a base and a rim per jaw, each from its own sketch, with region picks inside the sketched shapes
        let ext: Vec<_> = doc.features.iter().filter(|f| matches!(f.op, crate::doc::Op::Extrude { .. })).collect();
        assert_eq!(ext.len(), 4);
        for f in ext {
            let crate::doc::Op::Extrude { profiles, .. } = &f.op else { unreachable!() };
            assert!(!profiles.is_empty(), "{}", f.name);
        }
    }

    #[test]
    fn jaw_mesh_is_closed() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let j = jaws(&held_cam(), &part).unwrap();
        // closed and facing out: the signed volume is the blank's less the pocket's
        let (a, b) = j.block();
        let full = (b.x - a.x) * (b.y - a.y) * (j.top - j.bottom);
        let vol: f64 = block_tris(&j).iter().map(|t| t[0].dot(t[1].cross(t[2])) / 6.0).sum();
        let pocket = shapes_area(&crate::cam_3d::intersect(&rect(a.extend(0.0), b.extend(0.0)), &j.pocket)) * (j.top - j.floor);
        assert!((vol - (full - pocket)).abs() < 1.0, "{vol} vs {}", full - pocket);
    }

    #[test]
    fn insert_bar_jaws_have_screw_holes_and_sit_in_the_vise() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let mut c = held_cam();
        let (m, w, t) = cam::jaw_mount("gen3_insert_bar").unwrap();
        let sj = c.soft_jaws.as_mut().unwrap();
        (sj.width, sj.thickness, sj.height, sj.gap, sj.mount) = (w, t, 25.0, 26.0, Some(m.clone()));
        let j = jaws(&c, &part).unwrap();
        assert!(j.warnings.is_empty(), "{:?}", j.warnings);
        // four holes per jaw on the bar's centre line, 25.4 apart
        assert_eq!(j.holes[0].len(), 4);
        assert!((j.holes[0][0].y - (j.rects[0].1.y - 6.35)).abs() < 1e-9 && (j.holes[1][0].y - (j.rects[1].0.y + 6.35)).abs() < 1e-9);
        assert!((j.holes[0][1].x - j.holes[0][0].x - 25.4).abs() < 1e-9);
        // the mesh is closed: blank less pocket, through holes and counterbores
        let tris = jaw_tris(&j);
        let vol: f64 = tris.iter().map(|t| t[0].dot(t[1].cross(t[2])) / 6.0).sum();
        let h = j.top - j.bottom;
        let blanks: f64 = j.rects.iter().map(|(a, b)| (b.x - a.x) * (b.y - a.y) * h).sum();
        let pocket: f64 = j.rects.iter().map(|(a, b)| shapes_area(&crate::cam_3d::intersect(&rect(a.extend(0.0), b.extend(0.0)), &j.pocket))).sum::<f64>() * (j.top - j.floor);
        let disc = |d: f64| shapes_area(&vec![vec![circle(DVec2::ZERO, d / 2.0)]]);
        let holes = 8.0 * (disc(m.drill) * m.seat + disc(m.cbore) * (h - m.seat));
        // (the pocket and the counterbores overlap a little at the grip faces)
        let expect = blanks - pocket - holes;
        assert!(vol < expect + 30.0 && vol > expect - 30.0 - 8.0 * disc(m.cbore) * (j.top - j.floor), "{vol} vs {expect}");
        // the vise's lips in front of the jaws and its jaws behind are fixtures
        let bx = boxes(&j);
        let lip = bx.iter().find(|b| b.0 == "Vise lip (front)").unwrap();
        assert!((lip.1.y - j.rects[0].1.y).abs() < 1e-9 && (lip.2.y - j.rects[0].1.y - 1.9).abs() < 1e-9 && (lip.2.z - j.bottom - 4.57).abs() < 1e-9);
        assert!(bx.iter().any(|b| b.0 == "Vise jaw (back)" && b.1.y >= j.rects[1].1.y - 1e-9));
        // the printed jaws get the holes too
        c.soft_jaws.as_mut().unwrap().printed = true;
        let doc = print_doc(&jaws(&c, &part).unwrap());
        for n in ["Screw holes (front)", "Counterbores (back)"] {
            assert!(doc.features.iter().any(|f| f.name == n), "{n}");
        }
        // the wrong thickness for the channel, a gap the lips don't allow, too short for the screws
        let sj = c.soft_jaws.as_mut().unwrap();
        (sj.thickness, sj.gap, sj.height) = (20.0, 3.0, 9.0);
        let w = jaws(&c, &part).unwrap().warnings.clone();
        for k in ["12.7 mm thick", "at least 3.8 mm apart", "below the top of the vise's lip"] {
            assert!(w.iter().any(|x| x.contains(k)), "{k}: {w:?}");
        }
    }

    #[test]
    fn printed_block_in_one_piece() {
        let part = Part::new(&block(DVec3::ZERO, DVec3::new(60.0, 30.0, 10.0)), &[], "top");
        let mut c = held_cam();
        let sj = c.soft_jaws.as_mut().unwrap();
        (sj.printed, sj.one_piece, sj.gap, sj.mount) = (true, true, 1.0, cam::jaw_mount("gen3_insert_bar").map(|m| m.0));
        let j = jaws(&c, &part).unwrap();
        // no gap, no screw holes, nothing to warn about the bars it doesn't use
        assert!(j.one_piece && j.mount.is_none() && j.warnings.is_empty(), "{:?}", j.warnings);
        // the gap is a slit, open at the ends with R5 rounds, stopping 1.5 above the bottom
        let (a, b) = j.block();
        let mid = (j.rects[0].1.y + j.rects[1].0.y) / 2.0;
        assert!(inside(&j.slit, [a.x + 30.0, mid]) && !inside(&j.slit, [a.x + 30.0, j.rects[0].1.y - 0.5]));
        assert!(inside(&j.slit, [a.x + 0.3, j.rects[1].0.y + 0.3]) && !inside(&j.slit, [a.x + 3.0, j.rects[1].0.y + 3.0]));
        assert!((j.web - j.bottom - 1.5).abs() < 1e-9);
        // closed mesh: block less pocket less slit above the web
        let vol: f64 = jaw_tris(&j).iter().map(|t| t[0].dot(t[1].cross(t[2])) / 6.0).sum();
        let r = rect(a.extend(0.0), b.extend(0.0));
        let ar = |s: &Shapes| shapes_area(&crate::cam_3d::intersect(&r, s));
        let expect = shapes_area(&r) * (j.top - j.bottom) - ar(&j.slit) * (j.top - j.web) - ar(&difference(&j.pocket, &j.slit)) * (j.top - j.floor);
        assert!((vol - expect).abs() < 1.0, "{vol} vs {expect}");
        let doc = print_doc(&j);
        let names: Vec<&str> = doc.features.iter().filter(|f| matches!(f.op, crate::doc::Op::Extrude { .. })).map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Block web", "Block half (front)", "Block rim (front)", "Block half (back)", "Block rim (back)"]);
    }
}
