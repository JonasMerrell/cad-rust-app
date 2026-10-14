//! Orthographic trackball camera, Z up, matching the HTML viewport's controls:
//! right-drag orbits, middle-drag (or Shift+right-drag) pans, wheel zooms at the cursor.

use glam::{Mat4, Quat, Vec2, Vec3};

const DISTANCE: f32 = 20000.0;

#[derive(Clone, Debug)]
pub struct Camera {
    pub target: Vec3,
    /// Unit vector from the target towards the eye.
    pub back: Vec3,
    pub up: Vec3,
    /// Half of the visible height in world units.
    pub half_height: f32,
    /// Viewport size in points.
    pub size: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdView {
    Iso,
    Front,
    Top,
    Right,
    Back,
    Bottom,
    Left,
}

impl Default for Camera {
    fn default() -> Self {
        let mut c = Self { target: Vec3::ZERO, back: Vec3::ONE.normalize(), up: Vec3::Z, half_height: 110.0, size: Vec2::new(800.0, 600.0) };
        c.set_view(StdView::Iso);
        c.target = Vec3::new(40.0, 40.0, 30.0);
        c
    }
}

impl Camera {
    pub fn aspect(&self) -> f32 {
        (self.size.x / self.size.y.max(1.0)).max(1e-3)
    }

    /// World units per screen point.
    pub fn wpp(&self) -> f32 {
        2.0 * self.half_height / self.size.y.max(1.0)
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.back * DISTANCE
    }

    pub fn right(&self) -> Vec3 {
        self.up.cross(self.back).normalize()
    }

    pub fn view(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye(), self.target, self.up)
    }

    pub fn proj(&self) -> Mat4 {
        let h = self.half_height;
        let w = h * self.aspect();
        glam::camera::rh::proj::directx::orthographic(-w, w, -h, h, 1.0, DISTANCE * 2.0)
    }

    pub fn view_proj(&self) -> Mat4 {
        self.proj() * self.view()
    }

    /// World point to viewport-local screen point (origin top-left, points).
    pub fn project(&self, p: Vec3) -> Vec2 {
        let ndc = self.view_proj().project_point3(p);
        Vec2::new((ndc.x + 1.0) * 0.5 * self.size.x, (1.0 - ndc.y) * 0.5 * self.size.y)
    }

    /// Ray (origin, dir) through a viewport-local screen point.
    pub fn ray(&self, s: Vec2) -> (Vec3, Vec3) {
        let nx = s.x / self.size.x * 2.0 - 1.0;
        let ny = 1.0 - s.y / self.size.y * 2.0;
        let h = self.half_height;
        let w = h * self.aspect();
        let origin = self.eye() + self.right() * (nx * w) + self.up * (ny * h);
        (origin, -self.back)
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        let n = 0.008;
        let axis_x = self.back.cross(self.up).normalize() * -1.0;
        let q = Quat::from_axis_angle(self.up, -dx * n) * Quat::from_axis_angle(axis_x, -dy * n);
        self.back = (q * self.back).normalize();
        self.up = (q * self.up).normalize();
        // keep up orthogonal to back
        self.up = (self.up - self.back * self.up.dot(self.back)).normalize();
    }

    pub fn pan(&mut self, dx: f32, dy: f32) {
        let wpp = self.wpp();
        self.target += self.right() * (-dx * wpp) + self.up * (dy * wpp);
    }

    /// Zoom by `factor` (>1 zooms in) keeping the world point under `s` fixed.
    pub fn zoom_at(&mut self, s: Vec2, factor: f32) {
        let before = self.ray(s).0;
        self.half_height = (self.half_height / factor).clamp(1e-3, 1e6);
        let after = self.ray(s).0;
        let d = before - after;
        self.target += d - self.back * d.dot(self.back);
    }

    pub fn set_view(&mut self, v: StdView) {
        let (back, up) = match v {
            StdView::Iso => (Vec3::new(1.0, -1.0, 1.0).normalize(), Vec3::Z),
            StdView::Front => (-Vec3::Y, Vec3::Z),
            StdView::Back => (Vec3::Y, Vec3::Z),
            StdView::Top => (Vec3::Z, Vec3::Y),
            StdView::Bottom => (-Vec3::Z, -Vec3::Y),
            StdView::Right => (Vec3::X, Vec3::Z),
            StdView::Left => (-Vec3::X, Vec3::Z),
        };
        self.set_orientation(back, up);
    }

    pub fn set_orientation(&mut self, back: Vec3, up: Vec3) {
        self.back = back.normalize();
        self.up = (up - self.back * up.dot(self.back)).normalize();
    }

    /// Frame an axis-aligned box.
    pub fn fit(&mut self, min: Vec3, max: Vec3) {
        if !(min.is_finite() && max.is_finite()) || min.cmpgt(max).any() {
            return;
        }
        let center = (min + max) * 0.5;
        let r = ((max - min).length() * 0.5).max(1.0);
        self.target = center;
        let aspect = self.aspect();
        self.half_height = r * 1.15 / aspect.min(1.0);
    }
}
