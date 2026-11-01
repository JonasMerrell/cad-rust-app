//! The stock as it is between operations: a column of material [lo, hi] over each cell of a grid
//! (setup coordinates). Made by the simulator, used to skip air (rest machining), and carried from
//! one setup to the next by re-slicing it along the new spindle axis.

use glam::{DMat3, DVec3};

/// Height of an empty column.
pub const EMPTY: f32 = f32::NEG_INFINITY;

#[derive(Clone, Debug)]
pub struct StockMap {
    pub x0: f64,
    pub y0: f64,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    /// Top and bottom of the material over each cell centre; `hi` is EMPTY where there is none.
    pub hi: Vec<f32>,
    pub lo: Vec<f32>,
}

impl StockMap {
    /// A solid block.
    #[cfg(test)]
    pub fn block(lo: DVec3, hi: DVec3, cell: f64) -> StockMap {
        let nx = ((hi.x - lo.x) / cell).ceil() as usize + 1;
        let ny = ((hi.y - lo.y) / cell).ceil() as usize + 1;
        StockMap { x0: lo.x, y0: lo.y, cell, nx, ny, hi: vec![hi.z as f32; nx * ny], lo: vec![lo.z as f32; nx * ny] }
    }

    fn index(&self, x: f64, y: f64) -> Option<usize> {
        let i = ((x - self.x0) / self.cell).round();
        let j = ((y - self.y0) / self.cell).round();
        if i < 0.0 || j < 0.0 || i >= self.nx as f64 || j >= self.ny as f64 {
            return None;
        }
        Some(j as usize * self.nx + i as usize)
    }

    /// The material column over (x, y), if any.
    pub fn column(&self, x: f64, y: f64) -> Option<(f32, f32)> {
        let k = self.index(x, y)?;
        (self.hi[k] > self.lo[k]).then(|| (self.lo[k], self.hi[k]))
    }

    /// Top of the material over (x, y), EMPTY where there is none.
    pub fn top(&self, x: f64, y: f64) -> f32 {
        self.column(x, y).map(|c| c.1).unwrap_or(EMPTY)
    }

    /// Material at height z over (x, y), more than `tol` thick above it.
    pub fn solid(&self, x: f64, y: f64, z: f64, tol: f64) -> bool {
        self.column(x, y).map(|(lo, hi)| (hi as f64) > z + tol && (lo as f64) < z).unwrap_or(false)
    }

    /// Highest material within `r` of (x, y) (EMPTY where there is none).
    pub fn top_near(&self, x: f64, y: f64, r: f64) -> f64 {
        let n = (r / self.cell).ceil() as i64 + 1;
        let ci = ((x - self.x0) / self.cell).round() as i64;
        let cj = ((y - self.y0) / self.cell).round() as i64;
        let mut best = EMPTY;
        for j in (cj - n).max(0)..=(cj + n).min(self.ny as i64 - 1) {
            let dy = self.y0 + j as f64 * self.cell - y;
            for i in (ci - n).max(0)..=(ci + n).min(self.nx as i64 - 1) {
                let dx = self.x0 + i as f64 * self.cell - x;
                if dx * dx + dy * dy > (r + self.cell) * (r + self.cell) {
                    continue;
                }
                let k = j as usize * self.nx + i as usize;
                if self.hi[k] > self.lo[k] {
                    best = best.max(self.hi[k]);
                }
            }
        }
        best as f64
    }

    /// Highest material anywhere.
    pub fn max_top(&self) -> f64 {
        self.hi.iter().zip(&self.lo).filter(|(h, l)| h > l).map(|(h, _)| *h).fold(EMPTY, f32::max) as f64
    }

    /// How deep a tool with its tip at `p` cuts into the material: the thickest column of material
    /// inside its cutting surface (`lut`: height above the tip every `step` mm from the axis).
    pub fn engaged(&self, p: DVec3, r: f64, lut: &[f32], step: f64) -> f64 {
        if lut.is_empty() {
            return 0.0;
        }
        let n = (r / self.cell).ceil() as i64 + 1;
        let ci = ((p.x - self.x0) / self.cell).round() as i64;
        let cj = ((p.y - self.y0) / self.cell).round() as i64;
        let mut deepest = 0.0f32;
        let tip = p.z as f32;
        for j in (cj - n).max(0)..=(cj + n).min(self.ny as i64 - 1) {
            let dy = self.y0 + j as f64 * self.cell - p.y;
            for i in (ci - n).max(0)..=(ci + n).min(self.nx as i64 - 1) {
                let dx = self.x0 + i as f64 * self.cell - p.x;
                let d2 = dx * dx + dy * dy;
                if d2 > r * r {
                    continue;
                }
                let k = j as usize * self.nx + i as usize;
                let (lo, hi) = (self.lo[k], self.hi[k]);
                if hi <= lo {
                    continue;
                }
                let z = tip + lut[((d2.sqrt() / step) as usize).min(lut.len() - 1)];
                let d = hi - z.max(lo);
                if d > deepest {
                    deepest = d;
                }
            }
        }
        deepest as f64
    }

    /// The same material seen from another setup: `from` and `to` are the part → setup rotations.
    /// When the axes map onto each other (ORIENTS without tilt) the columns are re-sliced exactly;
    /// a tilted setup samples along each new column instead.
    pub fn reorient(&self, from: DMat3, to: DMat3) -> StockMap {
        let m = to * from.transpose();
        if (m - DMat3::IDENTITY).abs().to_cols_array().iter().all(|v| *v < 1e-9) {
            return self.clone();
        }
        let mt = m.transpose();
        let aligned = m.to_cols_array().iter().all(|v| v.abs() < 1e-9 || (v.abs() - 1.0).abs() < 1e-9);
        // bounds in the new setup
        let (zlo, zhi) = self.lo.iter().zip(&self.hi).filter(|(l, h)| h > l).fold((f64::INFINITY, f64::NEG_INFINITY), |a, (l, h)| (a.0.min(*l as f64), a.1.max(*h as f64)));
        if !zlo.is_finite() {
            return StockMap { x0: 0.0, y0: 0.0, cell: self.cell, nx: 1, ny: 1, hi: vec![EMPTY], lo: vec![0.0] };
        }
        let x1 = self.x0 + (self.nx - 1) as f64 * self.cell;
        let y1 = self.y0 + (self.ny - 1) as f64 * self.cell;
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for k in 0..8 {
            let p = m * DVec3::new([self.x0, x1][k & 1], [self.y0, y1][(k >> 1) & 1], [zlo, zhi][(k >> 2) & 1]);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let cell = self.cell;
        let nx = ((hi.x - lo.x) / cell).round() as usize + 1;
        let ny = ((hi.y - lo.y) / cell).round() as usize + 1;
        let mut out = StockMap { x0: lo.x, y0: lo.y, cell, nx, ny, hi: vec![EMPTY; nx * ny], lo: vec![0.0; nx * ny] };
        if !aligned {
            self.resample(&mut out, mt, lo.z, hi.z);
            return out;
        }
        // the new spindle axis in the old setup
        let a = mt * DVec3::Z;
        let axis = if a.z.abs() > 0.5 {
            2
        } else if a.x.abs() > 0.5 {
            0
        } else {
            1
        };
        let sign = a.to_array()[axis].signum();
        for j in 0..ny {
            for i in 0..nx {
                let (x, y) = (lo.x + i as f64 * cell, lo.y + j as f64 * cell);
                // the column's foot in the old setup; z along the column is sign * (old coordinate along `axis`)
                let b = mt * DVec3::new(x, y, 0.0);
                let k = j * nx + i;
                if axis == 2 {
                    if let Some((l, h)) = self.column(b.x, b.y) {
                        let (l, h) = (l as f64 * sign, h as f64 * sign);
                        out.lo[k] = l.min(h) as f32;
                        out.hi[k] = l.max(h) as f32;
                    }
                    continue;
                }
                // a horizontal line through the old grid at old height b.z
                let (mut first, mut last) = (f64::INFINITY, f64::NEG_INFINITY);
                let n = if axis == 0 { self.nx } else { self.ny };
                for s in 0..n {
                    let (px, py) = if axis == 0 { (self.x0 + s as f64 * cell, b.y) } else { (b.x, self.y0 + s as f64 * cell) };
                    let Some(kk) = self.index(px, py) else { continue };
                    let (l, h) = (self.lo[kk] as f64, self.hi[kk] as f64);
                    if h > l && b.z >= l && b.z <= h {
                        let c = if axis == 0 { px } else { py };
                        first = first.min(c);
                        last = last.max(c);
                    }
                }
                if first <= last {
                    let (l, h) = ((first - cell / 2.0) * sign, (last + cell / 2.0) * sign);
                    out.lo[k] = l.min(h) as f32;
                    out.hi[k] = l.max(h) as f32;
                }
            }
        }
        out
    }
}

impl StockMap {
    /// Fill `out` (a grid in the new setup) by walking down each of its columns through this
    /// material: `mt` takes new setup coordinates to this one's, `z0..z1` spans the material.
    fn resample(&self, out: &mut StockMap, mt: DMat3, z0: f64, z1: f64) {
        let step = self.cell * 0.5;
        let n = ((z1 - z0) / step).ceil() as usize + 1;
        for j in 0..out.ny {
            for i in 0..out.nx {
                let (x, y) = (out.x0 + i as f64 * out.cell, out.y0 + j as f64 * out.cell);
                let (mut first, mut last) = (f64::INFINITY, f64::NEG_INFINITY);
                for s in 0..n {
                    let z = z0 + s as f64 * step;
                    let b = mt * DVec3::new(x, y, z);
                    if let Some((l, h)) = self.column(b.x, b.y) {
                        if b.z >= l as f64 && b.z <= h as f64 {
                            first = first.min(z);
                            last = last.max(z);
                        }
                    }
                }
                if first <= last {
                    let k = j * out.nx + i;
                    out.lo[k] = (first - step / 2.0) as f32;
                    out.hi[k] = (last + step / 2.0) as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam_paths::orient_matrix;

    #[test]
    fn flips_and_turns() {
        // a 10 × 20 × 5 block in the top setup with a 2 mm step cut off the half x > 5
        let mut m = StockMap::block(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 20.0, 5.0), 0.5);
        for j in 0..m.ny {
            for i in 0..m.nx {
                if m.x0 + i as f64 * m.cell > 5.0 {
                    m.hi[j * m.nx + i] = 3.0;
                }
            }
        }
        let top = orient_matrix("top");
        // flipped over: the old bottom is the new top
        let b = m.reorient(top, orient_matrix("bottom"));
        let p = orient_matrix("bottom") * DVec3::new(2.0, 10.0, 0.0);
        assert_eq!(b.top(p.x, p.y), 0.0);
        let q = orient_matrix("bottom") * DVec3::new(8.0, 10.0, 0.0);
        assert_eq!(b.column(q.x, q.y), Some((-3.0, 0.0)));
        // from the right: the column through the stepped half stops 3 mm up
        let r = orient_matrix("right");
        let s = m.reorient(top, r);
        let lowp = r * DVec3::new(0.0, 10.0, 4.0);
        let (l, h) = s.column(lowp.x, lowp.y).unwrap();
        assert!((h - (r * DVec3::new(5.0, 10.0, 4.0)).z as f32).abs() < 0.6, "{l} {h}");
        let deep = r * DVec3::new(0.0, 10.0, 2.0);
        let (_, h2) = s.column(deep.x, deep.y).unwrap();
        assert!((h2 - (r * DVec3::new(10.0, 10.0, 2.0)).z as f32).abs() < 0.6);
    }

    #[test]
    fn tilted_setup_keeps_the_material() {
        // a 20 × 20 × 10 block seen from a setup tilted 20° about X: same volume, top raised on one side
        let m = StockMap::block(DVec3::ZERO, DVec3::new(20.0, 20.0, 10.0), 0.25);
        let to = DMat3::from_rotation_x(20f64.to_radians());
        let t = m.reorient(DMat3::IDENTITY, to);
        let vol = |s: &StockMap| s.hi.iter().zip(&s.lo).filter(|(h, l)| h > l).map(|(h, l)| (h - l) as f64).sum::<f64>() * s.cell * s.cell;
        let (v0, v1) = (vol(&m), vol(&t));
        assert!((v1 - v0).abs() / v0 < 0.03, "{v0} {v1}");
        // a point in the middle of the old top face is on top of the new column there
        let p = to * DVec3::new(10.0, 10.0, 10.0);
        let (_, h) = t.column(p.x, p.y).unwrap();
        assert!((h as f64 - p.z).abs() < 0.3, "{h} {}", p.z);
    }
}
