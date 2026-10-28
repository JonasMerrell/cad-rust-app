//! Offscreen rendering for scripts: shaded triangles with edge lines, translucency and an orthographic or
//! perspective camera, rasterized on the CPU in horizontal bands on several threads.
//!
//! A `Mesh` (from `Shape.mesh`) is tessellated once and can be drawn in many images, each time with its
//! own colour, opacity and offset (an axis jogged to a new position).

use pyo3::prelude::*;
use rayon::prelude::*;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn xf(m: &[f64; 12], p: V3) -> V3 {
    [m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3], m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7], m[8] * p[0] + m[9] * p[1] + m[10] * p[2] + m[11]]
}
fn rot(m: &[f64; 12], p: V3) -> V3 {
    [m[0] * p[0] + m[1] * p[1] + m[2] * p[2], m[4] * p[0] + m[5] * p[1] + m[6] * p[2], m[8] * p[0] + m[9] * p[1] + m[10] * p[2]]
}
fn unit(a: V3) -> V3 {
    let n = dot(a, a).sqrt();
    if n < 1e-300 { [0.0, 0.0, 1.0] } else { scale(a, 1.0 / n) }
}

/// A tessellated shape: triangles with smooth normals per CAD face, and its edges as polylines.
#[pyclass(module = "cadrust._core", frozen)]
pub struct Mesh {
    pub pts: Vec<V3>,
    pub normals: Vec<V3>,
    pub tris: Vec<[u32; 3]>,
    pub lines: Vec<Vec<V3>>,
}

#[pymethods]
impl Mesh {
    #[getter]
    fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// (xmin, ymin, zmin, xmax, ymax, zmax) of the triangles, or None when empty.
    fn bbox(&self) -> Option<(f64, f64, f64, f64, f64, f64)> {
        if self.pts.is_empty() {
            return None;
        }
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in &self.pts {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        Some((lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]))
    }
}

impl Mesh {
    /// From a triangulation (flat vertices, triangles, face of each triangle) and edge polylines:
    /// normals averaged over the corners a face's triangles share.
    pub fn build(verts: &[f64], tris: &[u32], faces: &[u32], lines: Vec<Vec<V3>>) -> Mesh {
        let pts: Vec<V3> = verts.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        let tris: Vec<[u32; 3]> = tris.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        // vertices are numbered per face by the triangulation, so a plain average stays within a face
        let mut normals = vec![[0.0; 3]; pts.len()];
        for t in &tris {
            let (a, b, c) = (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]);
            let n = cross(sub(b, a), sub(c, a));
            for &i in t {
                normals[i as usize] = add(normals[i as usize], n);
            }
        }
        let _ = faces;
        for n in normals.iter_mut() {
            *n = unit(*n);
        }
        Mesh { pts, normals, tris, lines }
    }
}

/// One mesh in an image.
pub struct Item<'a> {
    mesh: &'a Mesh,
    color: [f32; 3],
    opacity: f32,
    /// Rigid transform applied to the mesh (row-major 3 × 4).
    m: [f64; 12],
    edges: Option<([f32; 3], f32)>,
    shine: f32,
}

struct Camera {
    eye: V3,
    fwd: V3,
    right: V3,
    up: V3,
    /// Orthographic: the height of the view in mm; perspective: 0.
    ortho_h: f64,
    /// Perspective: 1 / tan(fov / 2).
    f: f64,
    w: usize,
    h: usize,
}

impl Camera {
    /// Pixel x, y and depth along the view.
    fn project(&self, p: V3) -> Option<V3> {
        let d = sub(p, self.eye);
        let z = dot(d, self.fwd);
        let aspect = self.w as f64 / self.h as f64;
        let (x, y) = if self.ortho_h > 0.0 {
            (dot(d, self.right) / (self.ortho_h * 0.5 * aspect), dot(d, self.up) / (self.ortho_h * 0.5))
        } else {
            if z <= 1e-6 {
                return None;
            }
            (dot(d, self.right) * self.f / (z * aspect), dot(d, self.up) * self.f / z)
        };
        Some([(x + 1.0) * 0.5 * self.w as f64, (1.0 - y) * 0.5 * self.h as f64, z])
    }
}

struct Band<'a> {
    y0: usize,
    rows: usize,
    col: &'a mut [[f32; 3]],
    depth: &'a mut [f32],
}

fn shade(n: V3, fwd: V3, light: V3, half: V3, base: [f32; 3], shine: f32) -> [f32; 3] {
    let n = if dot(n, fwd) > 0.0 { scale(n, -1.0) } else { n };
    let diff = dot(n, light).max(0.0) as f32;
    let spec = (dot(n, half).max(0.0) as f32).powf(24.0 + 40.0 * shine) * (0.15 + 0.45 * shine);
    let l = 0.5 + 0.5 * diff;
    [(base[0] * l + spec).min(1.0), (base[1] * l + spec).min(1.0), (base[2] * l + spec).min(1.0)]
}

#[allow(clippy::too_many_arguments)]
fn fill_tri(band: &mut Band, w: usize, s: [V3; 3], n: [V3; 3], color: [f32; 3], opacity: f32, shine: f32, cam: &Camera, light: V3, half: V3) {
    let [a, b, c] = s;
    let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
    if area.abs() < 1e-12 {
        return;
    }
    let ylo = band.y0 as f64;
    let yhi = (band.y0 + band.rows) as f64;
    let miny = a[1].min(b[1]).min(c[1]);
    let maxy = a[1].max(b[1]).max(c[1]);
    if maxy < ylo || miny >= yhi {
        return;
    }
    let x0 = a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize;
    let x1 = (a[0].max(b[0]).max(c[0]).ceil().max(0.0) as usize).min(w.saturating_sub(1));
    let y0 = (miny.floor().max(ylo)) as usize;
    let y1 = ((maxy.ceil()) as usize).min(band.y0 + band.rows - 1);
    if x0 > x1 || y0 > y1 {
        return;
    }
    for y in y0..=y1 {
        let py = y as f64 + 0.5;
        for x in x0..=x1 {
            let px = x as f64 + 0.5;
            let w0 = ((b[0] - px) * (c[1] - py) - (c[0] - px) * (b[1] - py)) / area;
            let w1 = ((c[0] - px) * (a[1] - py) - (a[0] - px) * (c[1] - py)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = (w0 * a[2] + w1 * b[2] + w2 * c[2]) as f32;
            let k = (y - band.y0) * w + x;
            if z >= band.depth[k] {
                continue;
            }
            let nn = unit(add(add(scale(n[0], w0), scale(n[1], w1)), scale(n[2], w2)));
            let s = shade(nn, cam.fwd, light, half, color, shine);
            if opacity >= 0.999 {
                band.depth[k] = z;
                band.col[k] = s;
            } else {
                for q in 0..3 {
                    band.col[k][q] = band.col[k][q] * (1.0 - opacity) + s[q] * opacity;
                }
            }
        }
    }
}

/// A thick line from a to b (pixels, with depths), depth-tested with a bias toward the camera.
fn draw_line(band: &mut Band, w: usize, a: V3, b: V3, color: [f32; 3], alpha: f32, width: f32, bias: f32) {
    let r = (width * 0.5).max(0.5) as f64;
    let ylo = band.y0 as f64 - r;
    let yhi = (band.y0 + band.rows) as f64 + r;
    if a[1].max(b[1]) < ylo || a[1].min(b[1]) > yhi {
        return;
    }
    let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let steps = (len / 0.5).ceil().max(1.0) as usize;
    let ri = r.ceil() as i64;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let (px, py, pz) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t);
        let (cx, cy) = (px.floor() as i64, py.floor() as i64);
        for dy in -ri..=ri {
            let y = cy + dy;
            if y < band.y0 as i64 || y >= (band.y0 + band.rows) as i64 {
                continue;
            }
            for dx in -ri..=ri {
                let x = cx + dx;
                if x < 0 || x >= w as i64 {
                    continue;
                }
                let (qx, qy) = (x as f64 + 0.5 - px, y as f64 + 0.5 - py);
                if qx * qx + qy * qy > r * r {
                    continue;
                }
                let k = (y as usize - band.y0) * w + x as usize;
                if pz as f32 - bias > band.depth[k] {
                    continue;
                }
                for q in 0..3 {
                    band.col[k][q] = band.col[k][q] * (1.0 - alpha) + color[q] * alpha;
                }
            }
        }
    }
}

/// Render the items; returns RGB bytes, row by row.
#[allow(clippy::too_many_arguments)]
fn render(items: &[Item], eye: V3, target: V3, up: V3, ortho_h: f64, fov: f64, w: usize, h: usize, bg: [f32; 3]) -> Vec<u8> {
    let fwd = unit(sub(target, eye));
    let right = unit(cross(fwd, up));
    let up = cross(right, fwd);
    let cam = Camera { eye, fwd, right, up, ortho_h, f: 1.0 / (fov.max(1.0).to_radians() / 2.0).tan(), w, h };
    let light = unit(add(add(scale(fwd, -1.0), scale(up, 0.35)), scale(right, 0.2)));
    let half = unit(sub(light, fwd));
    // project once
    let screen: Vec<Vec<Option<V3>>> = items
        .par_iter()
        .map(|it| it.mesh.pts.iter().map(|p| cam.project(xf(&it.m, *p))).collect())
        .collect();
    let lines: Vec<Vec<Vec<Option<V3>>>> = items
        .par_iter()
        .map(|it| if it.edges.is_some() { it.mesh.lines.iter().map(|l| l.iter().map(|p| cam.project(xf(&it.m, *p))).collect()).collect() } else { vec![] })
        .collect();
    // translucent triangles, far to near
    let mut glass: Vec<(f64, usize, usize)> = vec![];
    for (ii, it) in items.iter().enumerate() {
        if it.opacity < 0.999 {
            for (ti, t) in it.mesh.tris.iter().enumerate() {
                if let (Some(a), Some(b), Some(c)) = (screen[ii][t[0] as usize], screen[ii][t[1] as usize], screen[ii][t[2] as usize]) {
                    glass.push(((a[2] + b[2] + c[2]) / 3.0, ii, ti));
                }
            }
        }
    }
    glass.sort_by(|a, b| b.0.total_cmp(&a.0));
    let depth_scale = if ortho_h > 0.0 { ortho_h / h as f64 } else { 1.0 };
    let bias = (1.5 * depth_scale) as f32;
    let mut col = vec![bg; w * h];
    let mut depth = vec![f32::INFINITY; w * h];
    let rows = 32usize;
    let bands: Vec<Band> = col
        .chunks_mut(w * rows)
        .zip(depth.chunks_mut(w * rows))
        .enumerate()
        .map(|(i, (c, d))| Band { y0: i * rows, rows: c.len() / w, col: c, depth: d })
        .collect();
    bands.into_par_iter().for_each(|mut band| {
        let tri = |band: &mut Band, ii: usize, t: &[u32; 3]| {
            let it = &items[ii];
            let sp = &screen[ii];
            if let (Some(a), Some(b), Some(c)) = (sp[t[0] as usize], sp[t[1] as usize], sp[t[2] as usize]) {
                let n = [0, 1, 2].map(|k| rot(&it.m, it.mesh.normals[t[k] as usize]));
                fill_tri(band, w, [a, b, c], n, it.color, it.opacity, it.shine, &cam, light, half);
            }
        };
        let edges = |band: &mut Band, ii: usize| {
            if let Some((ec, ew)) = items[ii].edges {
                let alpha = if items[ii].opacity < 0.999 { (items[ii].opacity * 1.6).min(1.0) } else { 1.0 };
                for l in &lines[ii] {
                    for s in l.windows(2) {
                        if let (Some(a), Some(b)) = (s[0], s[1]) {
                            draw_line(band, w, a, b, ec, alpha, ew, bias);
                        }
                    }
                }
            }
        };
        for (ii, it) in items.iter().enumerate() {
            if it.opacity >= 0.999 {
                for t in &it.mesh.tris {
                    tri(&mut band, ii, t);
                }
            }
        }
        for (ii, it) in items.iter().enumerate() {
            if it.opacity >= 0.999 {
                edges(&mut band, ii);
            }
        }
        for &(_, ii, ti) in &glass {
            tri(&mut band, ii, &items[ii].mesh.tris[ti]);
        }
        for (ii, it) in items.iter().enumerate() {
            if it.opacity < 0.999 {
                edges(&mut band, ii);
            }
        }
    });
    let mut out = vec![0u8; w * h * 3];
    for (i, c) in col.iter().enumerate() {
        for k in 0..3 {
            out[3 * i + k] = (c[k] * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Render meshes to RGB bytes (row by row, top first).
///
/// items: (mesh, colour (r, g, b) 0–1, opacity 0–1, placement (row-major 3 × 4 rigid transform, or an offset
/// (x, y, z)), edge colour or None, edge width px, shine 0–1). The camera looks from `eye` toward `target` with `up` toward the top; `ortho_height` (mm)
/// gives an orthographic view of that height, otherwise `fov` (degrees) a perspective one.
#[pyfunction]
#[pyo3(signature = (items, eye, target, up, width, height, ortho_height=0.0, fov=30.0, background=(1.0, 1.0, 1.0)))]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn render_meshes(
    py: Python<'_>,
    items: Vec<(PyRef<Mesh>, (f32, f32, f32), f32, Vec<f64>, Option<(f32, f32, f32)>, f32, f32)>,
    eye: V3,
    target: V3,
    up: V3,
    width: usize,
    height: usize,
    ortho_height: f64,
    fov: f64,
    background: (f32, f32, f32),
) -> PyResult<pyo3::Py<pyo3::types::PyBytes>> {
    let mut list: Vec<Item> = vec![];
    for (m, c, o, pl, ec, ew, sh) in &items {
        let mat = match pl.len() {
            3 => [1.0, 0.0, 0.0, pl[0], 0.0, 1.0, 0.0, pl[1], 0.0, 0.0, 1.0, pl[2]],
            12 => std::array::from_fn(|i| pl[i]),
            _ => return Err(pyo3::exceptions::PyValueError::new_err("placement: an offset (3 values) or a 3 × 4 matrix (12)")),
        };
        list.push(Item { mesh: m, color: [c.0, c.1, c.2], opacity: *o, m: mat, edges: ec.map(|e| ([e.0, e.1, e.2], *ew)), shine: *sh });
    }
    let px = py.detach(|| render(&list, eye, target, up, ortho_height, fov, width, height, [background.0, background.1, background.2]));
    Ok(pyo3::types::PyBytes::new(py, &px).into())
}
