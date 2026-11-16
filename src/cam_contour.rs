//! Stock-aware contour roughing.
//!
//! A contour on its own follows the part's outline once per level. When the stock reaches further out than
//! that one pass can take (a square block round a part with rounded corners leaves much more at the corners
//! than along the sides), the pass would bury the tool there. So before the wall pass, extra passes further
//! out take the stock down to the wall in steps no wider than the operation's radial engagement limit,
//! outermost first, and only where they touch material: the parts of those passes in air are skipped.
//!
//! `check` then measures the radial engagement the finished moves of one level really take (on a grid of the
//! material at that level), so the operation can warn when it can't keep under the limit.

use glam::{DVec2, DVec3};

use crate::cam::{Operation, Tool};
use crate::cam_paths::{difference, inside, offset, rect, shapes_area, Contour, Ctx, Move, Shapes, P2};

/// Default radial engagement limit, as a fraction of the tool diameter.
pub const DEFAULT_MAX_ENGAGEMENT: f64 = 0.25;

/// Most a contour's passes may take radially, mm.
pub fn max_engagement(op: &Operation, tool: &Tool) -> f64 {
    op.max_engagement.unwrap_or(DEFAULT_MAX_ENGAGEMENT).clamp(0.01, 1.0) * tool.diameter
}

/// The material at height z that isn't the part (nor `leave` on its walls): what the operations before this one
/// left, or the stock block when nothing is known about them.
pub fn material(ctx: &Ctx, shadow: &Shapes, z: f64, leave: f64) -> Shapes {
    match &ctx.stock_in {
        // grown by half a cell: the outline runs through the cells' middles, the material reaches a little further
        Some(m) => offset(&crate::cam_3d::leftover(ctx, m, z, leave), m.cell * 0.5),
        None => {
            let (lo, hi) = ctx.stock;
            if z < lo.z - 1e-6 || z >= hi.z - 1e-6 {
                return vec![];
            }
            difference(&rect(lo, hi), &offset(shadow, leave.max(0.0)))
        }
    }
}

/// The shapes without their holes.
pub fn filled(s: &Shapes) -> Shapes {
    s.iter().filter(|sh| !sh.is_empty()).map(|sh| vec![sh[0].clone()]).collect()
}

/// Without slivers thinner than 2e.
fn opened(s: &Shapes, e: f64) -> Shapes {
    offset(&offset(s, -e), e)
}

fn intersect(a: &Shapes, b: &Shapes) -> Shapes {
    crate::cam_3d::intersect(a, b)
}

/// A piece of an extra pass: a whole loop (cut with a ramp like the wall pass) or an open run that starts and ends in air.
#[derive(Clone, Debug)]
pub struct Seg {
    pub pts: Vec<P2>,
    pub closed: bool,
}

/// One extra pass round the outline: the tool-centre offset from the part and its pieces.
#[derive(Clone, Debug)]
pub struct Pass {
    pub dist: f64,
    pub segs: Vec<Seg>,
}

/// The extra passes outside the wall pass at one level, outermost first. `shadow`: the part at this level;
/// `outside`: the material outside its outline; `off`: the wall pass's offset (tool radius + leave); `step`: the
/// most each pass may take. Err(reach) when the material reaches further than `max_band` beyond the wall
/// (cutting a part out of a bigger plate): then the contour can't keep its engagement down on its own.
pub fn outer_passes(shadow: &Shapes, outside: &Shapes, off: f64, r: f64, step: f64, climb: bool, max_band: f64) -> Result<Vec<Pass>, f64> {
    if outside.is_empty() || step <= 1e-6 {
        return Ok(vec![]);
    }
    let e = 0.02;
    let left = |d: f64| opened(&difference(outside, &offset(shadow, d)), e);
    // after the pass at d (and every pass further out) the tool has cleared everything further than d - r out;
    // a pass takes no more than `step` when nothing is left further than d - r + step out
    let mut n = 0usize;
    while shapes_area(&left(off + n as f64 * step - r + step)) > 1e-3 {
        n += 1;
        if n as f64 * step > max_band {
            return Err(max_band);
        }
    }
    let mut passes = vec![];
    for k in (1..=n).rev() {
        let d = off + k as f64 * step;
        // the material this pass takes: what is still there after the passes further out
        let band = if k == n { left(d - r) } else { intersect(&left(d - r), &offset(shadow, d - r + step)) };
        let band = opened(&band, e);
        if shapes_area(&band) < 1e-3 {
            continue;
        }
        let near = offset(&band, r + 0.3);
        let mut segs = vec![];
        for sh in offset(shadow, d) {
            let Some(c) = sh.first() else { continue };
            let l: Contour = if climb { c.iter().rev().copied().collect() } else { c.clone() };
            segs.extend(clip(&l, &near, (r * 0.2).clamp(0.05, 1.0), (4.0 * r).max(6.0)));
        }
        if !segs.is_empty() {
            passes.push(Pass { dist: d, segs });
        }
    }
    Ok(passes)
}

/// The pieces of the closed loop `l` inside `near`, each starting and ending one sample outside it.
/// Gaps shorter than `bridge` are cut through rather than retracting over them.
pub(crate) fn clip(l: &Contour, near: &Shapes, h: f64, bridge: f64) -> Vec<Seg> {
    let n0 = l.len();
    if n0 < 2 {
        return vec![];
    }
    // resample so that no edge is longer than h
    let mut pts: Vec<P2> = vec![];
    for i in 0..n0 {
        let (a, b) = (DVec2::from(l[i]), DVec2::from(l[(i + 1) % n0]));
        let k = (a.distance(b) / h).ceil().max(1.0) as usize;
        for s in 0..k {
            pts.push(a.lerp(b, s as f64 / k as f64).to_array());
        }
    }
    let n = pts.len();
    let mut ins: Vec<bool> = pts.iter().map(|p| inside(near, *p)).collect();
    if ins.iter().all(|b| *b) {
        return vec![Seg { pts: l.clone(), closed: true }];
    }
    if !ins.iter().any(|b| *b) {
        return vec![];
    }
    // bridge short gaps
    let seglen = |i: usize| DVec2::from(pts[i]).distance(DVec2::from(pts[(i + 1) % n]));
    let s0 = (0..n).find(|i| ins[*i] && !ins[(i + n - 1) % n]).unwrap_or(0);
    let mut i = 0;
    while i < n {
        let k = (s0 + i) % n;
        if !ins[k] {
            // a gap from k: how long?
            let mut j = 0;
            let mut len = 0.0;
            while j < n && !ins[(k + j) % n] {
                len += seglen((k + j + n - 1) % n);
                j += 1;
            }
            len += seglen((k + j + n - 1) % n);
            if len < bridge && j < n {
                for t in 0..j {
                    ins[(k + t) % n] = true;
                }
            }
            i += j.max(1);
        } else {
            i += 1;
        }
    }
    if ins.iter().all(|b| *b) {
        return vec![Seg { pts: l.clone(), closed: true }];
    }
    // runs of inside points, each with the outside point before and after it
    let start = (0..n).find(|i| !ins[*i]).unwrap();
    let mut segs = vec![];
    let mut cur: Vec<P2> = vec![];
    for t in 1..=n {
        let k = (start + t) % n;
        if ins[k] {
            if cur.is_empty() {
                cur.push(pts[(k + n - 1) % n]);
            }
            cur.push(pts[k]);
        } else if !cur.is_empty() {
            cur.push(pts[k]);
            segs.push(Seg { pts: std::mem::take(&mut cur), closed: false });
        }
    }
    segs
}

/// The highest radial engagement found, where (setup coordinates) and how many places go over the limit.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engagement {
    pub peak: f64,
    pub at: DVec3,
}

struct Grid {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    v: Vec<bool>,
}

impl Grid {
    /// Cells whose middles are inside the shapes (even-odd over all their loops), with a margin of `pad` round them.
    fn raster(s: &Shapes, cell: f64, pad: f64) -> Option<Grid> {
        let loops: Vec<&Contour> = s.iter().flat_map(|sh| sh.iter()).filter(|c| c.len() >= 3).collect();
        if loops.is_empty() {
            return None;
        }
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for p in loops.iter().flat_map(|c| c.iter()) {
            lo = lo.min(DVec2::from(*p));
            hi = hi.max(DVec2::from(*p));
        }
        lo -= DVec2::splat(pad);
        hi += DVec2::splat(pad);
        let nx = ((hi.x - lo.x) / cell).ceil() as usize + 1;
        let ny = ((hi.y - lo.y) / cell).ceil() as usize + 1;
        let mut v = vec![false; nx * ny];
        let mut xs: Vec<f64> = vec![];
        for j in 0..ny {
            let y = lo.y + (j as f64 + 0.5) * cell;
            xs.clear();
            for c in &loops {
                let m = c.len();
                for i in 0..m {
                    let (a, b) = (c[i], c[(i + 1) % m]);
                    if (a[1] <= y) != (b[1] <= y) {
                        xs.push(a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                    }
                }
            }
            xs.sort_by(|a, b| a.total_cmp(b));
            for w in xs.chunks_exact(2) {
                let i0 = ((w[0] - lo.x) / cell - 0.5).ceil().max(0.0) as usize;
                let i1 = (((w[1] - lo.x) / cell - 0.5).floor() as i64).min(nx as i64 - 1);
                for i in i0 as i64..=i1 {
                    v[j * nx + i as usize] = true;
                }
            }
        }
        Some(Grid { x0: lo.x, y0: lo.y, cell, nx, ny, v })
    }

    fn solid(&self, x: f64, y: f64) -> bool {
        let i = ((x - self.x0) / self.cell).floor();
        let j = ((y - self.y0) / self.cell).floor();
        i >= 0.0 && j >= 0.0 && (i as usize) < self.nx && (j as usize) < self.ny && self.v[j as usize * self.nx + i as usize]
    }

    /// Take the material within r of p. With the feed direction `t`, the radial engagement there: from the arc
    /// of the cutter's front edge in contact with material (angle φ), r (1 - cos φ), which is the width of a
    /// straight side cut and the diameter for a slot, and counts a cut entering the material at an angle by how
    /// much of the edge it loads. For a plunge (no direction), the width of what it takes in X.
    /// None when it touches nothing.
    fn take(&mut self, p: DVec2, r: f64, t: Option<DVec2>) -> Option<f64> {
        let c = self.cell;
        // the contact arc: material just outside the front half of the edge
        let contact = t.map(|t| {
            let n = t.perp();
            let rr = r + 0.5 * c;
            let k = ((std::f64::consts::PI * rr / (0.5 * c)).ceil() as usize).max(8);
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for s in 0..=k {
                let b = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * s as f64 / k as f64;
                let q = p + (t * b.cos() + n * b.sin()) * rr;
                if self.solid(q.x, q.y) {
                    lo = lo.min(b);
                    hi = hi.max(b);
                }
            }
            (hi >= lo).then_some(hi - lo)
        });
        let i0 = (((p.x - r - self.x0) / c - 0.5).floor().max(0.0)) as usize;
        let j0 = (((p.y - r - self.y0) / c - 0.5).floor().max(0.0)) as usize;
        let i1 = (((p.x + r - self.x0) / c - 0.5).ceil() as i64).min(self.nx as i64 - 1);
        let j1 = (((p.y + r - self.y0) / c - 0.5).ceil() as i64).min(self.ny as i64 - 1);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        if i1 >= 0 && j1 >= 0 {
            let r2 = r * r;
            for j in j0..=j1 as usize {
                let y = self.y0 + (j as f64 + 0.5) * c - p.y;
                for i in i0..=i1 as usize {
                    let k = j * self.nx + i;
                    if !self.v[k] {
                        continue;
                    }
                    let x = self.x0 + (i as f64 + 0.5) * c - p.x;
                    if x * x + y * y > r2 {
                        continue;
                    }
                    self.v[k] = false;
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
        }
        match contact {
            Some(phi) => {
                let phi = phi?;
                Some(if phi >= std::f64::consts::PI - 1e-9 { 2.0 * r } else { r * (1.0 - phi.cos()) })
            }
            None => (hi >= lo).then_some(hi - lo),
        }
    }
}

/// Grid size for checking a tool of radius r on material this big.
pub fn check_cell(r: f64, material: &Shapes) -> f64 {
    let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
    for p in material.iter().flat_map(|sh| sh.iter()).flat_map(|c| c.iter()) {
        lo = lo.min(DVec2::from(*p));
        hi = hi.max(DVec2::from(*p));
    }
    let s = (hi - lo + DVec2::splat(2.0 * r)).max(DVec2::ONE);
    (r / 16.0).max((s.x * s.y / 4.0e6).sqrt())
}

/// The radial engagement the moves take in the slab of material between z_lo and z_hi (`material`: its outline,
/// the same all the way up), sweeping a tool of radius r along them from `from` on a grid of `cell`.
/// Moves that stay at or above z_hi cut nothing. None when no move touches material.
pub fn check(moves: &[Move], from: Option<DVec3>, material: &Shapes, z_hi: f64, r: f64, cell: f64) -> Option<Engagement> {
    let mut g = Grid::raster(material, cell, r + cell)?;
    let mut pos = from;
    let mut best: Option<Engagement> = None;
    for m in moves {
        let q = m.end(pos.unwrap_or(DVec3::ZERO));
        let p = pos.unwrap_or(q);
        pos = Some(q);
        let Move::Feed(..) = m else { continue };
        if p.z.min(q.z) >= z_hi - 1e-3 {
            continue;
        }
        let d = (q - p).truncate();
        let len = d.length();
        let n = (len > 1e-9).then(|| d / len);
        let steps = (len / cell).ceil().max(1.0) as usize;
        // ramping in from above the slab: where it first gets into it, it only touches the top of the material
        // (the grid can't tell how deep), so that first bite isn't counted
        let mut seed = n.is_some() && p.z >= z_hi - 1e-3;
        for s in 1..=steps {
            let t = s as f64 / steps as f64;
            let c = p.lerp(q, t);
            let took = g.take(c.truncate(), r, n);
            if seed {
                if c.z < z_hi - 1e-3 {
                    seed = false;
                }
                continue;
            }
            if let Some(ae) = took {
                if best.map(|b| ae > b.peak).unwrap_or(true) {
                    best = Some(Engagement { peak: ae, at: c });
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_cut_and_slot_widths() {
        // a 20 × 20 block, tool Ø6 along a line 1 mm into its left side, then a slot through the middle
        let m: Shapes = vec![vec![vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]]]];
        let side = [Move::Feed([-2.0, -5.0, 0.0], 100.0), Move::Feed([-2.0, 25.0, 0.0], 100.0)];
        let e = check(&side, Some(DVec3::new(-2.0, -5.0, 0.0)), &m, 1.0, 3.0, 0.05).unwrap();
        assert!(e.peak <= 1.0 && e.peak > 0.85, "{e:?}");
        // ramping down into the side from above the slab: the first touch is only the top of it
        let ramp = [Move::Feed([-2.0, 25.0, 0.0], 100.0)];
        let e = check(&ramp, Some(DVec3::new(-2.0, 5.0, 1.0)), &m, 1.0, 3.0, 0.05).unwrap();
        assert!(e.peak <= 1.0 && e.peak > 0.85, "{e:?}");
        // a plunge straight down into it takes the whole width under the tool
        let plunge = [Move::Feed([10.0, 10.0, 0.0], 100.0)];
        let e = check(&plunge, Some(DVec3::new(10.0, 10.0, 2.0)), &m, 1.0, 3.0, 0.05).unwrap();
        assert!(e.peak > 5.8, "{e:?}");
        let slot = [Move::Feed([10.0, 25.0, 0.0], 100.0)];
        let e = check(&slot, Some(DVec3::new(10.0, -5.0, 0.0)), &m, 1.0, 3.0, 0.05).unwrap();
        assert!((e.peak - 6.0).abs() < 0.15, "{e:?}");
        // moves that stay above the slab cut nothing
        let above = [Move::Feed([10.0, 25.0, 1.0], 100.0)];
        assert!(check(&above, Some(DVec3::new(10.0, -5.0, 1.0)), &m, 1.0, 3.0, 0.05).is_none());
    }
}
