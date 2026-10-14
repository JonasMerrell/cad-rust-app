//! Text to closed outlines for sketches (glyph contours flattened to polylines).

use glam::DVec2;

struct Outliner {
    contours: Vec<Vec<DVec2>>,
    cur: Vec<DVec2>,
    scale: f64,
    origin: DVec2,
}

impl Outliner {
    fn p(&self, x: f32, y: f32) -> DVec2 {
        self.origin + DVec2::new(x as f64, y as f64) * self.scale
    }
    fn last(&self) -> DVec2 {
        *self.cur.last().unwrap_or(&self.origin)
    }
}

impl ttf_parser::OutlineBuilder for Outliner {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.cur.len() > 2 {
            self.contours.push(std::mem::take(&mut self.cur));
        }
        self.cur = vec![self.p(x, y)];
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.cur.push(p);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (a, b, c) = (self.last(), self.p(x1, y1), self.p(x, y));
        for i in 1..=6 {
            let t = i as f64 / 6.0;
            self.cur.push(a * (1.0 - t) * (1.0 - t) + b * 2.0 * t * (1.0 - t) + c * t * t);
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b, c, d) = (self.last(), self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        for i in 1..=8 {
            let t = i as f64 / 8.0;
            let u = 1.0 - t;
            self.cur.push(a * u * u * u + b * 3.0 * u * u * t + c * 3.0 * u * t * t + d * t * t * t);
        }
    }
    fn close(&mut self) {
        if self.cur.len() > 2 {
            // drop a repeated closing point
            if (self.cur[0] - *self.cur.last().unwrap()).length() < 1e-9 {
                self.cur.pop();
            }
            self.contours.push(std::mem::take(&mut self.cur));
        }
    }
}

/// Outlines of `text` with its baseline starting at `origin`, cap height ≈ `height`.
pub fn outlines(text: &str, origin: DVec2, height: f64) -> Vec<Vec<DVec2>> {
    let Ok(face) = ttf_parser::Face::parse(epaint_default_fonts::UBUNTU_LIGHT, 0) else { return vec![] };
    let cap = face.capital_height().unwrap_or(face.ascender()) as f64;
    let scale = height / cap.max(1.0);
    let mut o = Outliner { contours: vec![], cur: vec![], scale, origin };
    let mut x = 0.0f64;
    for ch in text.chars() {
        let Some(g) = face.glyph_index(ch) else {
            x += face.units_per_em() as f64 * 0.5;
            continue;
        };
        o.origin = origin + DVec2::new(x * scale, 0.0);
        face.outline_glyph(g, &mut o);
        if o.cur.len() > 2 {
            o.contours.push(std::mem::take(&mut o.cur));
        }
        x += face.glyph_hor_advance(g).unwrap_or(0) as f64;
    }
    o.contours
}

#[cfg(test)]
mod tests {
    #[test]
    fn letters_have_contours() {
        let c = super::outlines("Ao", glam::DVec2::ZERO, 10.0);
        // A: outer + hole, o: outer + hole
        assert_eq!(c.len(), 4, "{}", c.len());
        let maxy = c.iter().flatten().map(|p| p.y).fold(0.0, f64::max);
        assert!(maxy > 8.0 && maxy < 12.0, "{maxy}");
    }
}
