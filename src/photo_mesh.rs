//! The mesh half of photogrammetry (`photogrammetry.rs` runs the reconstruction tools): reading
//! the PLY meshes COLMAP and OpenMVS write, COLMAP's camera poses, finding the table the part
//! stood on, cutting the table and stray bits away, levelling the part onto XY, setting the
//! scale from two marks clicked on a ruler in a photo, and thinning the mesh down to a size
//! the modelling kernel can handle as a reference body.

use glam::{DMat3, DVec3};

/// A triangle mesh.
#[derive(Clone, Default, Debug)]
pub struct Mesh {
    pub v: Vec<DVec3>,
    pub t: Vec<[u32; 3]>,
}

impl Mesh {
    pub fn tri(&self, i: usize) -> [DVec3; 3] {
        let [a, b, c] = self.t[i];
        [self.v[a as usize], self.v[b as usize], self.v[c as usize]]
    }

    pub fn area(&self, i: usize) -> f64 {
        let [a, b, c] = self.tri(i);
        (b - a).cross(c - a).length() / 2.0
    }

    pub fn bbox(&self) -> (DVec3, DVec3) {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        for t in &self.t {
            for &i in t {
                lo = lo.min(self.v[i as usize]);
                hi = hi.max(self.v[i as usize]);
            }
        }
        (lo, hi)
    }

    /// Median edge length, a size for tolerances that follows the mesh's own detail.
    pub fn median_edge(&self) -> f64 {
        let step = (self.t.len() / 20000).max(1);
        let mut e: Vec<f64> = self.t.iter().step_by(step).map(|t| (self.v[t[0] as usize] - self.v[t[1] as usize]).length()).filter(|l| *l > 0.0).collect();
        if e.is_empty() {
            return 1.0;
        }
        let n = e.len() / 2;
        *e.select_nth_unstable_by(n, |a, b| a.total_cmp(b)).1
    }

    /// Keep only the triangles `keep` says yes to, dropping unused vertices.
    pub fn filter(&self, keep: impl Fn(usize) -> bool) -> Mesh {
        let mut map = vec![u32::MAX; self.v.len()];
        let mut out = Mesh::default();
        for (i, t) in self.t.iter().enumerate() {
            if !keep(i) {
                continue;
            }
            let mut n = [0u32; 3];
            for k in 0..3 {
                let v = t[k] as usize;
                if map[v] == u32::MAX {
                    map[v] = out.v.len() as u32;
                    out.v.push(self.v[v]);
                }
                n[k] = map[v];
            }
            out.t.push(n);
        }
        out
    }

    pub fn transformed(&self, m: DMat3, o: DVec3) -> Mesh {
        Mesh { v: self.v.iter().map(|p| m * (*p - o)).collect(), t: self.t.clone() }
    }

    /// Binary STL (for storing in an Import feature).
    pub fn to_stl(&self) -> Vec<u8> {
        let mut b = vec![0u8; 80];
        b[..21].copy_from_slice(b"rCAD photogrammetry  ");
        b.extend((self.t.len() as u32).to_le_bytes());
        for i in 0..self.t.len() {
            let [p, q, r] = self.tri(i);
            let n = (q - p).cross(r - p).normalize_or_zero();
            for v in [n, p, q, r] {
                for c in [v.x, v.y, v.z] {
                    b.extend((c as f32).to_le_bytes());
                }
            }
            b.extend([0, 0]);
        }
        b
    }

    /// The first triangle hit by a ray (distance along `d`, which needn't be unit length).
    pub fn raycast(&self, o: DVec3, d: DVec3) -> Option<f64> {
        let mut best: Option<f64> = None;
        for i in 0..self.t.len() {
            let [a, b, c] = self.tri(i);
            let (e1, e2) = (b - a, c - a);
            let p = d.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-18 {
                continue;
            }
            let s = o - a;
            let u = s.dot(p) / det;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = d.dot(q) / det;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let t = e2.dot(q) / det;
            if t > 0.0 && best.is_none_or(|b| t < b) {
                best = Some(t);
            }
        }
        best
    }
}

// ------------------------------------------------------------------ PLY

/// Read a PLY mesh (ASCII or binary): its vertex positions and faces (polygons are fanned
/// into triangles). A file with no faces is an error, since a point cloud isn't a mesh.
pub fn read_ply(b: &[u8]) -> Result<Mesh, String> {
    let m = parse_ply(b)?;
    if m.t.is_empty() {
        return Err("the PLY file has no faces (a point cloud, not a mesh)".into());
    }
    Ok(m)
}

/// Read the points of a PLY file (a point cloud such as COLMAP's `fused.ply`).
pub fn read_points(b: &[u8]) -> Result<Vec<DVec3>, String> {
    Ok(parse_ply(b)?.v)
}

fn parse_ply(b: &[u8]) -> Result<Mesh, String> {
    let end = find(b, b"end_header").ok_or("not a PLY file (no end_header)")?;
    let mut body = end + b"end_header".len();
    while body < b.len() && b[body] != b'\n' {
        body += 1;
    }
    body += 1;
    let header = String::from_utf8_lossy(&b[..end]);
    if !header.starts_with("ply") {
        return Err("not a PLY file".into());
    }
    #[derive(Debug)]
    enum Prop {
        Scalar(String, String),
        List(String, String, String),
    }
    let mut format = "ascii".to_string();
    let mut elems: Vec<(String, usize, Vec<Prop>)> = vec![];
    for l in header.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["format", f, ..] => format = f.to_string(),
            ["element", n, c] => elems.push((n.to_string(), c.parse().map_err(|_| "bad PLY element count")?, vec![])),
            ["property", "list", ct, it, n] => elems.last_mut().ok_or("PLY property before element")?.2.push(Prop::List(ct.to_string(), it.to_string(), n.to_string())),
            ["property", t, n] => elems.last_mut().ok_or("PLY property before element")?.2.push(Prop::Scalar(t.to_string(), n.to_string())),
            _ => {}
        }
    }
    let size = |t: &str| -> Result<usize, String> {
        Ok(match t {
            "char" | "uchar" | "int8" | "uint8" => 1,
            "short" | "ushort" | "int16" | "uint16" => 2,
            "int" | "uint" | "float" | "int32" | "uint32" | "float32" => 4,
            "double" | "float64" => 8,
            _ => return Err(format!("unknown PLY type {t}")),
        })
    };
    let big = format == "binary_big_endian";
    let ascii = format == "ascii";
    let mut pos = body;
    let mut words = if ascii { std::str::from_utf8(&b[body..]).map_err(|_| "the PLY text isn't UTF-8")?.split_ascii_whitespace() } else { "".split_ascii_whitespace() };
    let mut read = |t: &str| -> Result<f64, String> {
        if ascii {
            return words.next().ok_or("the PLY file is cut short")?.parse::<f64>().map_err(|_| "bad number in the PLY file".to_string());
        }
        let n = size(t)?;
        let s = b.get(pos..pos + n).ok_or("the PLY file is cut short")?;
        pos += n;
        let mut a = [0u8; 8];
        a[..n].copy_from_slice(s);
        if big {
            a[..n].reverse();
        }
        Ok(match t {
            "char" | "int8" => a[0] as i8 as f64,
            "uchar" | "uint8" => a[0] as f64,
            "short" | "int16" => i16::from_le_bytes([a[0], a[1]]) as f64,
            "ushort" | "uint16" => u16::from_le_bytes([a[0], a[1]]) as f64,
            "int" | "int32" => i32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            "uint" | "uint32" => u32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            "float" | "float32" => f32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            _ => f64::from_le_bytes(a),
        })
    };
    let mut m = Mesh::default();
    for (name, count, props) in &elems {
        let xyz: Vec<Option<usize>> = ["x", "y", "z"].iter().map(|c| props.iter().position(|p| matches!(p, Prop::Scalar(_, n) if n == c))).collect();
        for _ in 0..*count {
            let mut p = [0.0; 3];
            for (k, pr) in props.iter().enumerate() {
                match pr {
                    Prop::Scalar(t, _) => {
                        let x = read(t)?;
                        if name == "vertex" {
                            for c in 0..3 {
                                if xyz[c] == Some(k) {
                                    p[c] = x;
                                }
                            }
                        }
                    }
                    Prop::List(ct, it, pn) => {
                        let n = read(ct)? as usize;
                        let mut ids = Vec::with_capacity(n);
                        for _ in 0..n {
                            ids.push(read(it)? as u32);
                        }
                        if name == "face" && (pn == "vertex_indices" || pn == "vertex_index") {
                            for j in 1..n.saturating_sub(1) {
                                m.t.push([ids[0], ids[j], ids[j + 1]]);
                            }
                        }
                    }
                }
            }
            if name == "vertex" {
                m.v.push(DVec3::from(p));
            }
        }
    }
    let nv = m.v.len() as u32;
    m.t.retain(|t| t.iter().all(|&i| i < nv) && t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
    Ok(m)
}

/// Keep the triangles that lie near the points the surface was made from. Poisson meshing
/// closes the surface, so where the photos saw nothing (over the top of the part, under the
/// table, round the edges of the scan) it bulges out into smooth skins with no points under
/// them, or only a thin mist of stray points. `size` is how near counts: a grid of cells
/// that size (grown until a typical point shares its cell with a few dozen others), where a
/// cell counts as seen when it holds at least 5% of the points a typical point's cell holds.
pub fn trim_to_points(m: &Mesh, pts: &[DVec3], size: f64) -> Mesh {
    use std::collections::HashMap;
    if pts.is_empty() || !(size > 0.0) {
        return m.clone();
    }
    let mut size = size;
    let mut count: HashMap<(i64, i64, i64), u32>;
    let cell = |p: DVec3, s: f64| ((p.x / s).floor() as i64, (p.y / s).floor() as i64, (p.z / s).floor() as i64);
    let mut tries = 0;
    let typical = loop {
        count = HashMap::new();
        for p in pts {
            *count.entry(cell(*p, size)).or_default() += 1;
        }
        // the cell count a typical point sees (the point-weighted median)
        let mut c: Vec<u32> = count.values().cloned().collect();
        c.sort_unstable();
        let half = pts.len() as u64 / 2;
        let mut acc = 0u64;
        let typical = c.iter().find(|&&k| {
            acc += k as u64;
            acc >= half
        });
        let typical = typical.cloned().unwrap_or(1);
        tries += 1;
        if typical >= 40 || tries >= 5 {
            break typical;
        }
        size *= 1.6;
    };
    let min = ((typical as f64 * 0.35).ceil() as u32).max(1);
    let near = |p: DVec3| {
        let (x, y, z) = cell(p, size);
        (-1..=1).any(|i| (-1..=1).any(|j| (-1..=1).any(|k| count.get(&(x + i, y + j, z + k)).is_some_and(|&n| n >= min))))
    };
    m.filter(|i| near(m.tri(i).iter().sum::<DVec3>() / 3.0))
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Binary little-endian PLY (for keeping the full-detail mesh next to the photos).
pub fn write_ply(m: &Mesh) -> Vec<u8> {
    let mut b = format!("ply\nformat binary_little_endian 1.0\ncomment rCAD photogrammetry\nelement vertex {}\nproperty float x\nproperty float y\nproperty float z\nelement face {}\nproperty list uchar int vertex_indices\nend_header\n", m.v.len(), m.t.len()).into_bytes();
    for p in &m.v {
        for c in [p.x, p.y, p.z] {
            b.extend((c as f32).to_le_bytes());
        }
    }
    for t in &m.t {
        b.push(3);
        for &i in t {
            b.extend((i as i32).to_le_bytes());
        }
    }
    b
}

// ------------------------------------------------------------------ COLMAP cameras

/// A photo's pinhole camera from COLMAP (the undistorted model `image_undistorter` writes).
#[derive(Clone, Debug)]
pub struct Camera {
    pub name: String,
    /// World to camera: `x_cam = r * x + t`.
    pub r: DMat3,
    pub t: DVec3,
    /// fx, fy, cx, cy (pixels, origin at the image's top-left corner).
    pub k: [f64; 4],
}

impl Camera {
    pub fn centre(&self) -> DVec3 {
        -(self.r.transpose() * self.t)
    }

    /// The ray through an image position (pixels): origin and direction, in model space.
    pub fn ray(&self, u: f64, v: f64) -> (DVec3, DVec3) {
        let [fx, fy, cx, cy] = self.k;
        let d = self.r.transpose() * DVec3::new((u - cx) / fx, (v - cy) / fy, 1.0);
        (self.centre(), d.normalize())
    }
}

/// Read COLMAP's binary model (cameras.bin and images.bin in `dir`). Only pinhole cameras are
/// supported, which is what `image_undistorter` writes.
pub fn read_cameras(dir: &std::path::Path) -> Result<Vec<Camera>, String> {
    let cb = std::fs::read(dir.join("cameras.bin")).map_err(|e| format!("can't read the camera model: {e}"))?;
    let ib = std::fs::read(dir.join("images.bin")).map_err(|e| format!("can't read the photo poses: {e}"))?;
    struct Rd<'a>(&'a [u8], usize);
    impl Rd<'_> {
        fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
            let s = self.0.get(self.1..self.1 + N).ok_or("the COLMAP model is cut short")?;
            self.1 += N;
            Ok(s.try_into().unwrap())
        }
        fn u32(&mut self) -> Result<u32, String> {
            Ok(u32::from_le_bytes(self.take()?))
        }
        fn u64(&mut self) -> Result<u64, String> {
            Ok(u64::from_le_bytes(self.take()?))
        }
        fn f64(&mut self) -> Result<f64, String> {
            Ok(f64::from_le_bytes(self.take()?))
        }
    }
    let mut r = Rd(&cb, 0);
    let mut cams = std::collections::HashMap::new();
    for _ in 0..r.u64()? {
        let id = r.u32()?;
        let model = r.u32()?;
        let (w, h) = (r.u64()?, r.u64()?);
        // parameter counts of COLMAP's camera models, by id
        let np = [3, 4, 4, 5, 8, 8, 12, 5, 4, 5, 12].get(model as usize).copied().ok_or("unknown COLMAP camera model")?;
        let p: Vec<f64> = (0..np).map(|_| r.f64()).collect::<Result<_, _>>()?;
        let k = match model {
            0 => [p[0], p[0], p[1], p[2]],
            1 => [p[0], p[1], p[2], p[3]],
            _ => return Err("the photos' camera model has lens distortion: use the undistorted model".into()),
        };
        let _ = (w, h);
        cams.insert(id, k);
    }
    let mut r = Rd(&ib, 0);
    let mut out = vec![];
    for _ in 0..r.u64()? {
        let _id = r.u32()?;
        let q: Vec<f64> = (0..4).map(|_| r.f64()).collect::<Result<_, _>>()?;
        let t = DVec3::new(r.f64()?, r.f64()?, r.f64()?);
        let cam = r.u32()?;
        let mut name = vec![];
        loop {
            let [c] = r.take::<1>()?;
            if c == 0 {
                break;
            }
            name.push(c);
        }
        let n = r.u64()? as usize;
        r.1 += n * 24;
        let (w, x, y, z) = (q[0], q[1], q[2], q[3]);
        let rot = DMat3::from_cols(
            DVec3::new(1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)),
            DVec3::new(2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)),
            DVec3::new(2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)),
        );
        let k = *cams.get(&cam).ok_or("a photo names a camera the model doesn't have")?;
        out.push(Camera { name: String::from_utf8_lossy(&name).into_owned(), r: rot, t, k });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// ------------------------------------------------------------------ table plane

/// A plane: points p with `n · p = d`.
#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub n: DVec3,
    pub d: f64,
}

impl Plane {
    pub fn dist(&self, p: DVec3) -> f64 {
        self.n.dot(p) - self.d
    }
}

/// The biggest flat area of the mesh (the table the part stood on), by RANSAC over triangle
/// area, refined by a least-squares fit. Its normal points to `up_hint` (the cameras' side)
/// when given, otherwise to the side with more of the mesh.
pub fn find_table(m: &Mesh, tol: f64, up_hint: Option<DVec3>) -> Option<Plane> {
    if m.t.len() < 10 {
        return None;
    }
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut rnd = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let step = (m.t.len() / 60000).max(1);
    let sample: Vec<usize> = (0..m.t.len()).step_by(step).collect();
    let areas: Vec<f64> = sample.iter().map(|&i| m.area(i)).collect();
    let centres: Vec<DVec3> = sample.iter().map(|&i| m.tri(i).iter().sum::<DVec3>() / 3.0).collect();
    let mut best: Option<(f64, Plane)> = None;
    for _ in 0..400 {
        let [a, b, c] = [rnd(m.v.len()), rnd(m.v.len()), rnd(m.v.len())].map(|i| m.v[i]);
        let n = (b - a).cross(c - a);
        if n.length() < 1e-12 {
            continue;
        }
        let n = n.normalize();
        let pl = Plane { n, d: n.dot(a) };
        let score: f64 = centres.iter().zip(&areas).filter(|(p, _)| pl.dist(**p).abs() < tol).map(|(_, a)| a).sum();
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, pl));
        }
    }
    let (_, mut pl) = best?;
    // refine: plane through the inliers' centroid, normal = smallest principal axis
    // then tighten the band to the inliers' own spread, so the bottoms of the part's sides
    // don't tilt the fit
    let mut band = tol;
    for _ in 0..4 {
        let ins: Vec<(DVec3, f64)> = centres.iter().zip(&areas).filter(|(p, _)| pl.dist(**p).abs() < band).map(|(p, a)| (*p, *a)).collect();
        let w: f64 = ins.iter().map(|(_, a)| a).sum();
        if w <= 0.0 {
            break;
        }
        let c = ins.iter().map(|(p, a)| *p * *a).sum::<DVec3>() / w;
        let mut cov = DMat3::ZERO;
        for (p, a) in &ins {
            let d = *p - c;
            cov += DMat3::from_cols(d * d.x, d * d.y, d * d.z) * *a;
        }
        let n = smallest_eigvec(cov);
        let n = if n.dot(pl.n) < 0.0 { -n } else { n };
        pl = Plane { n, d: n.dot(c) };
        let rms = (ins.iter().map(|(p, a)| pl.dist(*p).powi(2) * a).sum::<f64>() / w).sqrt();
        band = band.min((3.0 * rms).max(tol * 0.02));
    }
    let up = match up_hint {
        Some(u) => pl.dist(u) >= 0.0,
        None => {
            let (mut above, mut below) = (0.0, 0.0);
            for (p, a) in centres.iter().zip(&areas) {
                let d = pl.dist(*p);
                if d > 3.0 * tol {
                    above += a;
                } else if d < -3.0 * tol {
                    below += a;
                }
            }
            above >= below
        }
    };
    if !up {
        pl = Plane { n: -pl.n, d: -pl.d };
    }
    Some(pl)
}

fn smallest_eigvec(m: DMat3) -> DVec3 {
    // inverse iteration on the shifted matrix (symmetric positive semi-definite)
    let tr = m.x_axis.x + m.y_axis.y + m.z_axis.z;
    let shifted = m + DMat3::from_diagonal(DVec3::splat(tr * 1e-9 + 1e-30));
    if shifted.determinant().abs() < 1e-300 {
        // singular: any null direction works; take the cross product of two rows
        for (a, b) in [(m.x_axis, m.y_axis), (m.y_axis, m.z_axis), (m.x_axis, m.z_axis)] {
            let c = a.cross(b);
            if c.length() > 1e-12 {
                return c.normalize();
            }
        }
        return DVec3::Z;
    }
    let inv = shifted.inverse();
    let mut v = DVec3::new(0.3, 0.5, 0.8).normalize();
    for _ in 0..50 {
        v = (inv * v).normalize();
    }
    v
}

/// Largest principal direction of 2D points (for lining the part up with X).
fn main_axis_2d(pts: &[(f64, f64, f64)]) -> f64 {
    let w: f64 = pts.iter().map(|p| p.2).sum();
    if w <= 0.0 {
        return 0.0;
    }
    let cx = pts.iter().map(|p| p.0 * p.2).sum::<f64>() / w;
    let cy = pts.iter().map(|p| p.1 * p.2).sum::<f64>() / w;
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for (x, y, a) in pts {
        let (dx, dy) = (x - cx, y - cy);
        xx += dx * dx * a;
        yy += dy * dy * a;
        xy += dx * dy * a;
    }
    0.5 * (2.0 * xy).atan2(xx - yy)
}

/// Triangles in connected pieces (sharing vertices): a piece index per triangle and the
/// number of pieces.
pub fn pieces(m: &Mesh) -> (Vec<u32>, usize) {
    let mut parent: Vec<u32> = (0..m.v.len() as u32).collect();
    fn root(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    for t in &m.t {
        let a = root(&mut parent, t[0]);
        for &v in &t[1..] {
            let b = root(&mut parent, v);
            if a != b {
                parent[b as usize] = a;
            }
        }
    }
    let mut ids = std::collections::HashMap::new();
    let out: Vec<u32> =
        m.t.iter()
            .map(|t| {
                let r = root(&mut parent, t[0]);
                let n = ids.len() as u32;
                *ids.entry(r).or_insert(n)
            })
            .collect();
    (out, ids.len())
}

/// What to do with a reconstructed mesh before it goes into a Part Studio.
#[derive(Clone, Debug)]
pub struct Cleanup {
    /// Stand the part on XY: the table plane becomes Z = 0, the part centred on the origin
    /// and its longest footprint direction along X.
    pub level: bool,
    /// Cut away the table and keep only the part (the biggest piece above the table, with
    /// any pieces close by that are big enough to matter).
    pub remove_table: bool,
    /// Thin the mesh down to about this many triangles (0: keep them all).
    pub triangles: usize,
    /// A block of known size standing on the table beside the part (a 1-2-3 block, a gauge
    /// block, a box; a ball works too, as d × d × d), in mm: found among the pieces on the
    /// table to set the scale, then left out of the result.
    pub reference: Option<[f64; 3]>,
}

impl Default for Cleanup {
    fn default() -> Self {
        Cleanup { level: true, remove_table: true, triangles: 40000, reference: None }
    }
}

/// The cleaned mesh and what was done.
pub struct Cleaned {
    pub mesh: Mesh,
    pub table: Option<Plane>,
    /// Millimetres per model unit, from the reference block when it was found.
    pub scale: Option<f64>,
    pub notes: Vec<String>,
}

/// The smallest rectangle around 2D points (by trying directions a degree apart): its sides.
fn footprint(pts: &[(f64, f64)]) -> [f64; 2] {
    let mut best = [f64::INFINITY, f64::INFINITY];
    for k in 0..90 {
        let a = (k as f64).to_radians();
        let (c, s) = (a.cos(), a.sin());
        let (mut x0, mut x1, mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for (x, y) in pts {
            let (u, v) = (c * x + s * y, -s * x + c * y);
            x0 = x0.min(u);
            x1 = x1.max(u);
            y0 = y0.min(v);
            y1 = y1.max(v);
        }
        if (x1 - x0) * (y1 - y0) < best[0] * best[1] {
            best = [x1 - x0, y1 - y0];
        }
    }
    best
}

/// How well a piece's box (footprint sides and height) matches the reference's sizes: the
/// scale it implies, the spread of the per-side scales (0 = a perfect match), and whether
/// only the footprint was used (the height can be off on a soft surface such as carpet,
/// where the floor plane sits a few mm from where the block stands).
fn reference_fit(fp: [f64; 2], h: f64, reference: [f64; 3]) -> (f64, f64, bool) {
    let spread = |r: &[f64]| {
        let mean = r.iter().sum::<f64>() / r.len() as f64;
        (r.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - r.iter().cloned().fold(f64::INFINITY, f64::min)) / mean
    };
    let mut best = (1.0, f64::INFINITY, false);
    let mut fp = fp;
    fp.sort_by(f64::total_cmp);
    // which of the block's sides stands up
    for up in 0..3 {
        let mut flat: Vec<f64> = (0..3).filter(|&i| i != up).map(|i| reference[i]).collect();
        flat.sort_by(f64::total_cmp);
        let r = [flat[0] / fp[0].max(1e-300), flat[1] / fp[1].max(1e-300), reference[up] / h.max(1e-300)];
        let all = spread(&r);
        if all < best.1 {
            best = (r.iter().sum::<f64>() / 3.0, all, false);
        }
        // footprint alone, when the height is within 25%
        let two = spread(&r[..2]);
        let hs = (r[2] / ((r[0] + r[1]) / 2.0) - 1.0).abs();
        if hs < 0.25 && two * 2.0 < best.1 {
            best = ((r[0] + r[1]) / 2.0, two * 2.0, true);
        }
    }
    best
}

/// Where the cameras look: the point closest to all their viewing axes, and a radius that
/// takes in what a photo shows around it (less than the cameras' distance).
pub fn focus(cams: &[(DVec3, DVec3)]) -> Option<(DVec3, f64)> {
    if cams.len() < 3 {
        return None;
    }
    let mut a = DMat3::ZERO;
    let mut b = DVec3::ZERO;
    for (c, d) in cams {
        let d = d.normalize();
        let m = DMat3::IDENTITY - DMat3::from_cols(d * d.x, d * d.y, d * d.z);
        a += m;
        b += m * *c;
    }
    if a.determinant().abs() < 1e-12 {
        return None;
    }
    let p = a.inverse() * b;
    let mut dist: Vec<f64> = cams.iter().map(|(c, _)| (*c - p).length()).collect();
    dist.sort_by(f64::total_cmp);
    Some((p, 0.6 * dist[dist.len() / 2]))
}

/// Soft floors (carpet, a cloth) stand up from the fitted plane in a ragged skirt round the
/// part, joined to it where it stands. Drop what stays low (under 4% of the part's height)
/// and lies outside the footprint of the part's higher surface.
fn trim_skirt(m: &Mesh, pl: Plane, r0: DMat3, cut: f64, edge: f64, notes: &mut Vec<String>) -> Mesh {
    use std::collections::HashSet;
    let h = m.v.iter().map(|p| pl.dist(*p)).fold(0.0, f64::max);
    let low = (2.0 * cut).max(0.04 * h);
    let cell = edge.max(0.04 * h);
    if !(cell > 0.0) {
        return m.clone();
    }
    let key = |p: DVec3| {
        let q = r0 * p;
        ((q.x / cell).floor() as i64, (q.y / cell).floor() as i64)
    };
    let mut over: HashSet<(i64, i64)> = HashSet::new();
    for p in m.v.iter().filter(|p| pl.dist(**p) > low) {
        over.insert(key(*p));
    }
    if over.is_empty() {
        return m.clone();
    }
    let inside = |p: DVec3| {
        let (x, y) = key(p);
        (-1..=1).any(|i| (-1..=1).any(|j| over.contains(&(x + i, y + j))))
    };
    let out = m.filter(|i| {
        let t = m.tri(i);
        t.iter().any(|p| pl.dist(*p) > low) || inside(t.iter().sum::<DVec3>() / 3.0)
    });
    let gone: f64 = (0..m.t.len()).map(|i| m.area(i)).sum::<f64>() - (0..out.t.len()).map(|i| out.area(i)).sum::<f64>();
    if out.t.len() < m.t.len() {
        notes.push(format!("dropped a low skirt round the part (soft floor), {:.0}% of its surface", 100.0 * gone / (0..m.t.len()).map(|i| m.area(i)).sum::<f64>().max(1e-300)));
    }
    out
}

/// `cams` are the photos' camera centres and viewing directions (empty when unknown): with
/// them the mesh is first cropped to where the cameras look, so a scan of a whole room keeps
/// the part in the middle, and the table found is the one under it.
pub fn clean(m: &Mesh, c: &Cleanup, cams: &[(DVec3, DVec3)]) -> Cleaned {
    let mut notes = vec![];
    let up_hint = (!cams.is_empty()).then(|| cams.iter().map(|(p, _)| *p).sum::<DVec3>() / cams.len() as f64);
    let focus = focus(cams);
    let cropped;
    let m = match focus {
        Some((p, r)) if c.remove_table => {
            cropped = m.filter(|i| m.tri(i).iter().all(|q| (*q - p).length() < r));
            if cropped.t.len() < m.t.len() {
                notes.push(format!("kept the {:.0}% of the mesh the cameras look at", 100.0 * cropped.t.len() as f64 / m.t.len().max(1) as f64));
            }
            if cropped.t.len() > 100 { &cropped } else { m }
        }
        _ => m,
    };
    let edge = m.median_edge();
    let table = if c.level || c.remove_table || c.reference.is_some() { find_table(m, edge, up_hint) } else { None };
    if (c.level || c.remove_table) && table.is_none() {
        notes.push("couldn't find the table: left the mesh as it was".into());
    }
    let mut mesh = m.clone();
    let mut scale = None;
    // the table's own frame (z up from the table)
    let r0 = table.map(|pl| {
        let z = pl.n;
        let x0 = if z.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
        let x = (x0 - z * x0.dot(z)).normalize();
        DMat3::from_cols(x, z.cross(x), z).transpose()
    });
    if let (Some(pl), Some(r0)) = (table, r0) {
        // above the table by more than the mesh's noise there
        // (a robust spread: the area-weighted median over the triangles, so the bottoms of
        // the part's sides don't count)
        let mut near: Vec<(f64, f64)> = (0..m.t.len()).map(|i| (pl.dist(m.tri(i).iter().sum::<DVec3>() / 3.0).abs(), m.area(i))).filter(|(d, _)| *d < edge).collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = near.iter().map(|x| x.1).sum::<f64>() / 2.0;
        let mut acc = 0.0;
        let med = near.iter().find(|x| {
            acc += x.1;
            acc >= half
        });
        let rms = 1.4826 * med.map(|x| x.0).unwrap_or(0.0);
        let cut = (4.0 * rms).max(0.05 * edge);
        let above = m.filter(|i| m.tri(i).iter().all(|p| pl.dist(*p) > cut));
        let (piece, n) = pieces(&above);
        let mut area = vec![0.0; n];
        let mut centre = vec![DVec3::ZERO; n];
        for i in 0..above.t.len() {
            let a = above.area(i);
            area[piece[i] as usize] += a;
            centre[piece[i] as usize] += above.tri(i).iter().sum::<DVec3>() / 3.0 * a;
        }
        for k in 0..n {
            centre[k] /= area[k].max(1e-300);
        }
        let mut skip = vec![false; n];
        if let Some(reference) = c.reference {
            // each sizeable piece's box: footprint on the table and height above it
            let mut verts: Vec<Vec<DVec3>> = vec![vec![]; n];
            for (i, t) in above.t.iter().enumerate() {
                verts[piece[i] as usize].extend(t.iter().map(|&v| r0 * above.v[v as usize]));
            }
            let best = (0..n)
                .filter(|&k| verts[k].len() > 30)
                .map(|k| {
                    // the height from the top face (a high percentile, past stray points) and the
                    // footprint from the middle of the sides: a reconstructed block flares out
                    // where it meets the table and rounds over at the top
                    let mut zs: Vec<f64> = verts[k].iter().map(|p| p.z - pl.d).collect();
                    zs.sort_by(f64::total_cmp);
                    let h = zs[(zs.len() as f64 * 0.995) as usize];
                    let mid: Vec<(f64, f64)> = verts[k].iter().filter(|p| (p.z - pl.d) > 0.3 * h && (p.z - pl.d) < 0.8 * h).map(|p| (p.x, p.y)).collect();
                    let fp = footprint(&if mid.len() > 10 { mid } else { verts[k].iter().map(|p| (p.x, p.y)).collect() });
                    let (s, mut spread, flat) = reference_fit(fp, h, reference);
                    // and it must look like a box: its surface about that of the block's top
                    // and four sides (a lump of carpet or a bit of the part can have the right
                    // bounding box but far more or less surface)
                    let [a, b, c] = reference.map(|x| x / s);
                    let full = 2.0 * (a * b + b * c + a * c);
                    if !(area[k] > 0.45 * full && area[k] < 1.6 * full) {
                        spread = spread.max(1.0);
                    }
                    (k, s, spread, flat, [fp[0], fp[1], h])
                })
                .min_by(|a, b| a.2.total_cmp(&b.2));
            match best {
                Some((k, s, spread, flat, _)) if spread < 0.06 => {
                    scale = Some(s);
                    skip[k] = true;
                    notes.push(format!(
                        "found the {:.1} × {:.1} × {:.1} mm reference ({} agree within {:.1}%): {s:.4} mm per unit",
                        reference[0],
                        reference[1],
                        reference[2],
                        if flat { "its footprint's sides" } else { "its sides" },
                        spread * 100.0
                    ));
                }
                Some((_, _, spread, _, size)) => notes.push(format!(
                    "couldn't find the reference block (the closest piece, {:.3} × {:.3} × {:.3} units, is {:.0}% off): set the scale another way",
                    size[0],
                    size[1],
                    size[2],
                    spread * 100.0
                )),
                None => notes.push("couldn't find the reference block: set the scale another way".into()),
            }
        }
        if c.remove_table {
            // the part: big and near the middle of where the cameras look (the biggest piece
            // alone can be a bit of floor or furniture in a room-sized scan)
            let score = |k: usize| match focus {
                Some((p, r)) => {
                    let d = centre[k] - p;
                    let off = (d - pl.n * d.dot(pl.n)).length() / (0.35 * r);
                    area[k] * (-off * off).exp()
                }
                None => area[k],
            };
            if let Some(big) = (0..n).filter(|k| !skip[*k]).max_by(|a, b| score(*a).total_cmp(&score(*b))) {
                // how far the biggest piece reaches from its centre
                let reach = (0..above.t.len()).filter(|&i| piece[i] as usize == big).flat_map(|i| above.tri(i)).map(|p| (p - centre[big]).length()).fold(0.0, f64::max);
                let keep: Vec<bool> = (0..n).map(|k| !skip[k] && (k == big || (area[k] > 0.02 * area[big] && (centre[k] - centre[big]).length() < 1.5 * reach))).collect();
                let kept = keep.iter().filter(|k| **k).count();
                mesh = above.filter(|i| keep[piece[i] as usize]);
                notes.push(format!("cut away the table and {} loose bits", n - kept - skip.iter().filter(|s| **s).count()));
                mesh = trim_skirt(&mesh, pl, r0, cut, edge, &mut notes);
            }
        }
    }
    if let (Some(pl), Some(r0), true) = (table, r0, c.level) {
        // footprint: the part's area-weighted points in the table's plane
        let pts: Vec<(f64, f64, f64)> = (0..mesh.t.len())
            .map(|i| {
                let p = r0 * (mesh.tri(i).iter().sum::<DVec3>() / 3.0);
                (p.x, p.y, mesh.area(i))
            })
            .collect();
        let ang = main_axis_2d(&pts);
        let r = DMat3::from_rotation_z(-ang) * r0;
        // origin: under the middle of the part's footprint, on the table
        let pm = mesh.transformed(r, DVec3::ZERO);
        let (lo, hi) = pm.bbox();
        let mid = (lo + hi) / 2.0;
        mesh = mesh.transformed(r, r.transpose() * DVec3::new(mid.x, mid.y, pl.d));
        notes.push("stood the part on XY".into());
    }
    if c.triangles > 0 && mesh.t.len() > c.triangles {
        let before = mesh.t.len();
        mesh = decimate(&mesh, c.triangles);
        notes.push(format!("thinned {before} triangles to {}", mesh.t.len()));
    }
    Cleaned { mesh, table, scale, notes }
}

// ------------------------------------------------------------------ scale

/// The scale (millimetres per model unit) from two marks clicked on a photo: each click's ray
/// hits the mesh (or the table, for marks on a ruler lying on it), and the two hits are
/// `mm` apart.
pub fn scale_from_clicks(m: &Mesh, table: Option<Plane>, cam: &Camera, a: [f64; 2], b: [f64; 2], mm: f64) -> Result<f64, String> {
    let hit = |uv: [f64; 2]| -> Result<DVec3, String> {
        let (o, d) = cam.ray(uv[0], uv[1]);
        let on_mesh = m.raycast(o, d);
        // a ruler on the table: the table plane is smoother than the mesh there
        let on_table = table.and_then(|pl| {
            let den = pl.n.dot(d);
            (den.abs() > 1e-12).then(|| (pl.d - pl.n.dot(o)) / den).filter(|t| *t > 0.0)
        });
        let t = match (on_mesh, on_table) {
            (Some(tm), Some(tt)) if (tm - tt).abs() < 0.02 * tt => tt,
            (Some(tm), _) => tm,
            (None, Some(tt)) => tt,
            (None, None) => return Err("the click doesn't land on the scanned model".into()),
        };
        Ok(o + d * t)
    };
    let (pa, pb) = (hit(a)?, hit(b)?);
    let d = (pa - pb).length();
    if !(d > 0.0) || !(mm > 0.0) {
        return Err("pick two different marks and a distance above zero".into());
    }
    Ok(mm / d)
}

// ------------------------------------------------------------------ decimation

/// Thin a mesh to about `target` triangles by quadric error edge collapse (Garland and
/// Heckbert), keeping open edges in place and never folding a triangle over.
pub fn decimate(m: &Mesh, target: usize) -> Mesh {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    type Q = [f64; 10];
    fn plane_q(n: DVec3, d: f64, w: f64) -> Q {
        let (a, b, c) = (n.x, n.y, n.z);
        [a * a * w, a * b * w, a * c * w, a * d * w, b * b * w, b * c * w, b * d * w, c * c * w, c * d * w, d * d * w]
    }
    fn add(a: &mut Q, b: &Q) {
        for i in 0..10 {
            a[i] += b[i];
        }
    }
    fn err(q: &Q, p: DVec3) -> f64 {
        let (x, y, z) = (p.x, p.y, p.z);
        q[0] * x * x + 2.0 * q[1] * x * y + 2.0 * q[2] * x * z + 2.0 * q[3] * x + q[4] * y * y + 2.0 * q[5] * y * z + 2.0 * q[6] * y + q[7] * z * z + 2.0 * q[8] * z + q[9]
    }
    let nv = m.v.len();
    let mut pos = m.v.clone();
    let mut tris = m.t.clone();
    let mut dead = vec![false; tris.len()];
    let mut vfaces: Vec<Vec<u32>> = vec![vec![]; nv];
    for (i, t) in tris.iter().enumerate() {
        for &v in t {
            vfaces[v as usize].push(i as u32);
        }
    }
    let mut q: Vec<Q> = vec![[0.0; 10]; nv];
    let mut edges: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    for t in &tris {
        let [a, b, c] = t.map(|i| pos[i as usize]);
        let n = (b - a).cross(c - a);
        let area = n.length() / 2.0;
        if area <= 0.0 {
            continue;
        }
        let n = n.normalize();
        let pq = plane_q(n, -n.dot(a), area);
        for &v in t {
            add(&mut q[v as usize], &pq);
        }
        for k in 0..3 {
            let (u, v) = (t[k], t[(k + 1) % 3]);
            *edges.entry((u.min(v), u.max(v))).or_default() += 1;
        }
    }
    // open edges: a steep plane through the edge, at right angles to its face, holds them
    for t in &tris {
        let [a, b, c] = t.map(|i| pos[i as usize]);
        let fnrm = (b - a).cross(c - a).normalize_or_zero();
        for k in 0..3 {
            let (u, v) = (t[k], t[(k + 1) % 3]);
            if edges.get(&(u.min(v), u.max(v))) == Some(&1) {
                let e = pos[v as usize] - pos[u as usize];
                let n = e.cross(fnrm).normalize_or_zero();
                if n != DVec3::ZERO {
                    let pq = plane_q(n, -n.dot(pos[u as usize]), e.length_squared() * 10.0);
                    add(&mut q[u as usize], &pq);
                    add(&mut q[v as usize], &pq);
                }
            }
        }
    }
    let mut ver = vec![0u32; nv];
    let mut gone = vec![false; nv];
    let best_point = |q: &Q, a: DVec3, b: DVec3| -> (f64, DVec3) {
        let m3 = DMat3::from_cols(DVec3::new(q[0], q[1], q[2]), DVec3::new(q[1], q[4], q[5]), DVec3::new(q[2], q[5], q[7]));
        let mut cands = vec![a, b, (a + b) / 2.0];
        if m3.determinant().abs() > 1e-12 * (q[0] + q[4] + q[7]).powi(3).max(1e-300) {
            let p = m3.inverse() * DVec3::new(-q[3], -q[6], -q[8]);
            // stay near the edge (an ill-conditioned optimum can fly off)
            if (p - (a + b) / 2.0).length() < 2.0 * (a - b).length() {
                cands.push(p);
            }
        }
        cands.into_iter().map(|p| (err(q, p).max(0.0), p)).min_by(|x, y| x.0.total_cmp(&y.0)).unwrap()
    };
    #[derive(PartialEq, PartialOrd)]
    struct C(f64);
    impl Eq for C {}
    impl Ord for C {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            self.0.total_cmp(&o.0)
        }
    }
    let mut heap: BinaryHeap<Reverse<(C, u32, u32, u32, u32)>> = BinaryHeap::new();
    let push = |heap: &mut BinaryHeap<Reverse<(C, u32, u32, u32, u32)>>, q: &[Q], pos: &[DVec3], ver: &[u32], u: u32, v: u32| {
        let mut s = q[u as usize];
        add(&mut s, &q[v as usize]);
        let (c, _) = best_point(&s, pos[u as usize], pos[v as usize]);
        heap.push(Reverse((C(c), u, v, ver[u as usize], ver[v as usize])));
    };
    for &(u, v) in edges.keys() {
        push(&mut heap, &q, &pos, &ver, u, v);
    }
    let mut live = tris.len();
    let neighbours = |vf: &[u32], tris: &[[u32; 3]], dead: &[bool], me: u32| -> Vec<u32> {
        let mut n: Vec<u32> = vf.iter().filter(|&&f| !dead[f as usize]).flat_map(|&f| tris[f as usize]).filter(|&x| x != me).collect();
        n.sort_unstable();
        n.dedup();
        n
    };
    while live > target {
        let Some(Reverse((_, u, v, vu, vv))) = heap.pop() else { break };
        if gone[u as usize] || gone[v as usize] || ver[u as usize] != vu || ver[v as usize] != vv {
            continue;
        }
        let (ui, vi) = (u as usize, v as usize);
        let nu = neighbours(&vfaces[ui], &tris, &dead, u);
        let nvv = neighbours(&vfaces[vi], &tris, &dead, v);
        if !nu.contains(&v) {
            continue;
        }
        // link condition: the shared neighbours are just the edge's faces' far corners
        let shared: Vec<u32> = nu.iter().filter(|x| nvv.contains(x)).copied().collect();
        let edge_faces: Vec<u32> = vfaces[ui].iter().filter(|&&f| !dead[f as usize] && tris[f as usize].contains(&v)).copied().collect();
        if shared.len() != edge_faces.len() || edge_faces.is_empty() {
            continue;
        }
        let mut s = q[ui];
        add(&mut s, &q[vi]);
        let (_, p) = best_point(&s, pos[ui], pos[vi]);
        // no face may flip or collapse to a sliver
        let mut ok = true;
        for &w in &[u, v] {
            for &f in &vfaces[w as usize] {
                let f = f as usize;
                if dead[f] || edge_faces.contains(&(f as u32)) {
                    continue;
                }
                let t = tris[f];
                let old = t.map(|i| pos[i as usize]);
                let new = t.map(|i| if i == u || i == v { p } else { pos[i as usize] });
                let n0 = (old[1] - old[0]).cross(old[2] - old[0]);
                let n1 = (new[1] - new[0]).cross(new[2] - new[0]);
                if n1.length_squared() <= 0.0 || n0.normalize_or_zero().dot(n1.normalize_or_zero()) < 0.3 {
                    ok = false;
                    break;
                }
            }
            if !ok {
                break;
            }
        }
        if !ok {
            continue;
        }
        // collapse v into u
        for &f in &edge_faces {
            dead[f as usize] = true;
            live -= 1;
        }
        let vf = std::mem::take(&mut vfaces[vi]);
        for f in vf {
            if dead[f as usize] {
                continue;
            }
            for x in tris[f as usize].iter_mut() {
                if *x == v {
                    *x = u;
                }
            }
            vfaces[ui].push(f);
        }
        vfaces[ui].retain(|&f| !dead[f as usize]);
        pos[ui] = p;
        q[ui] = s;
        gone[vi] = true;
        ver[ui] += 1;
        for w in neighbours(&vfaces[ui], &tris, &dead, u) {
            push(&mut heap, &q, &pos, &ver, u.min(w), u.max(w));
        }
    }
    let out = Mesh { v: pos, t: tris };
    out.filter(|i| !dead[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Weld the faces' shared edges, as a reconstructed mesh is.
    fn weld(m: &Mesh) -> Mesh {
        let mut ids = std::collections::HashMap::new();
        let mut v = vec![];
        let map: Vec<u32> =
            m.v.iter()
                .map(|p| {
                    let k = (p * 1e6).round().as_i64vec3().to_array();
                    *ids.entry(k).or_insert_with(|| {
                        v.push(*p);
                        v.len() as u32 - 1
                    })
                })
                .collect();
        let t: Vec<[u32; 3]> = m.t.iter().map(|t| t.map(|i| map[i as usize])).filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).collect();
        Mesh { v, t }
    }

    /// A box standing on a big flat table, finely triangulated, with a tilt and offset.
    fn quad(m: &mut Mesh, o: DVec3, u: DVec3, v: DVec3, n: usize) {
        let base = m.v.len() as u32;
        for j in 0..=n {
            for i in 0..=n {
                m.v.push(o + u * (i as f64 / n as f64) + v * (j as f64 / n as f64));
            }
        }
        let w = n as u32 + 1;
        for j in 0..n as u32 {
            for i in 0..n as u32 {
                let a = base + j * w + i;
                m.t.push([a, a + 1, a + w + 1]);
                m.t.push([a, a + w + 1, a + w]);
            }
        }
    }

    /// Four sides and a top of a box standing on z = 0 (outward normals).
    fn add_box(m: &mut Mesh, x0: f64, y0: f64, x1: f64, y1: f64, h: f64, n: usize) {
        let (w, d) = (x1 - x0, y1 - y0);
        quad(m, DVec3::new(x0, y0, h), DVec3::X * w, DVec3::Y * d, n);
        quad(m, DVec3::new(x0, y0, 0.0), DVec3::X * w, DVec3::Z * h, n);
        quad(m, DVec3::new(x1, y1, 0.0), -DVec3::X * w, DVec3::Z * h, n);
        quad(m, DVec3::new(x0, y1, 0.0), -DVec3::Y * d, DVec3::Z * h, n);
        quad(m, DVec3::new(x1, y0, 0.0), DVec3::Y * d, DVec3::Z * h, n);
    }

    /// A box standing on a big flat table, finely triangulated, as if reconstructed: tilted,
    /// moved and at 1/50 scale. With `block`, a 1-2-3 block lies flat beside it.
    fn scene_with(block: bool) -> (Mesh, DMat3, DVec3) {
        let mut m = Mesh::default();
        // table 200 x 200 at z = 0
        quad(&mut m, DVec3::new(-100.0, -100.0, 0.0), DVec3::X * 200.0, DVec3::Y * 200.0, 60);
        // box 40 x 20 x 10 at (10, 5)
        add_box(&mut m, -10.0, -5.0, 30.0, 15.0, 10.0, 20);
        if block {
            add_box(&mut m, -95.0, -90.0, -95.0 + 76.2, -90.0 + 50.8, 25.4, 20);
        }
        let m = weld(&m);
        let r = DMat3::from_rotation_x(0.4) * DMat3::from_rotation_y(-0.3);
        let o = DVec3::new(3.0, -2.0, 7.0);
        let s = 1.0 / 50.0;
        (Mesh { v: m.v.iter().map(|p| r * *p * s + o).collect(), t: m.t }, r, o)
    }

    fn scene() -> (Mesh, DMat3, DVec3) {
        scene_with(false)
    }

    #[test]
    fn finds_the_reference_block_for_scale() {
        let (m, _, _) = scene_with(true);
        let c = clean(&m, &Cleanup { reference: Some([25.4, 50.8, 76.2]), triangles: 0, ..Default::default() }, &[]);
        let s = c.scale.expect("found the block");
        assert!((s - 50.0).abs() < 0.5, "{s} {:?}", c.notes);
        // the block is left out: the result is just the 40 x 20 box
        let (lo, hi) = c.mesh.bbox();
        let size = (hi - lo) * s;
        assert!((size.x - 40.0).abs() < 0.5 && (size.y - 20.0).abs() < 0.5, "{size:?}");
        // a wrong reference isn't matched
        let c = clean(&m, &Cleanup { reference: Some([10.0, 10.0, 10.0]), triangles: 0, ..Default::default() }, &[]);
        assert!(c.scale.is_none());
    }

    #[test]
    fn a_lump_with_the_blocks_bounding_box_isnt_the_block() {
        // where the block would be, a thin cross of two walls: the right footprint and height,
        // but far less surface than a box
        let mut m = Mesh::default();
        quad(&mut m, DVec3::new(-100.0, -100.0, 0.0), DVec3::X * 200.0, DVec3::Y * 200.0, 60);
        add_box(&mut m, -10.0, -5.0, 30.0, 15.0, 10.0, 20);
        quad(&mut m, DVec3::new(-95.0, -90.0, 0.0), DVec3::new(76.2, 50.8, 0.0), DVec3::Z * 25.4, 20);
        quad(&mut m, DVec3::new(-95.0, -90.0 + 50.8, 0.0), DVec3::new(76.2, -50.8, 0.0), DVec3::Z * 25.4, 20);
        let c = clean(&weld(&m), &Cleanup { reference: Some([25.4, 50.8, 76.2]), triangles: 0, ..Default::default() }, &[]);
        assert!(c.scale.is_none(), "{:?}", c.notes);
    }

    #[test]
    fn drops_a_carpet_skirt_round_the_part() {
        // the part stands on a ragged disc of carpet pile, joined to it, a little above the floor
        let mut m = Mesh::default();
        quad(&mut m, DVec3::new(-100.0, -100.0, 0.0), DVec3::X * 200.0, DVec3::Y * 200.0, 120);
        add_box(&mut m, -10.0, -5.0, 30.0, 15.0, 40.0, 20);
        let n = m.v.len();
        quad(&mut m, DVec3::new(-30.0, -25.0, 0.8), DVec3::X * 80.0, DVec3::Y * 60.0, 40);
        for p in &mut m.v[n..] {
            p.z += 0.4 * ((p.x * 0.7).sin() * (p.y * 0.9).cos());
        }
        let c = clean(&weld(&m), &Cleanup { triangles: 0, ..Default::default() }, &[]);
        let (lo, hi) = c.mesh.bbox();
        let size = hi - lo;
        assert!(size.x < 50.0 && size.y < 30.0 && (size.z - 40.0).abs() < 1.0, "{size:?} {:?}", c.notes);
    }

    #[test]
    fn trims_the_surface_to_the_points() {
        // a closed box surface, but the points only cover its sides (the top was never seen)
        let mut m = Mesh::default();
        add_box(&mut m, 0.0, 0.0, 10.0, 10.0, 10.0, 10);
        // densely sampled where seen, plus a thin mist of stray points over the top
        let mut pts: Vec<DVec3> = vec![];
        for p in m.v.iter().filter(|p| p.z < 9.0) {
            for k in 0..50 {
                pts.push(*p + DVec3::splat(0.002 * k as f64));
            }
        }
        for x in 0..10 {
            pts.push(DVec3::new(x as f64 + 0.5, 5.0, 10.0));
        }
        let t = trim_to_points(&m, &pts, 0.5);
        let (_, hi) = t.bbox();
        assert!(hi.z < 10.0 && t.t.len() > m.t.len() / 2, "{} of {}", t.t.len(), m.t.len());
        // a point cloud reads as points
        let mut b = b"ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nend_header\n".to_vec();
        b.extend(b"1 2 3 9\n4 5 6 9\n");
        assert_eq!(read_points(&b).unwrap(), vec![DVec3::new(1.0, 2.0, 3.0), DVec3::new(4.0, 5.0, 6.0)]);
        assert!(read_ply(&b).is_err());
    }

    /// A room-sized scan: a small part and a reference block on a floor, with a wall, a big
    /// piece of furniture and floor clutter around; cameras circle the part.
    #[test]
    fn room_scan_keeps_the_part_the_cameras_look_at() {
        let mut m = Mesh::default();
        quad(&mut m, DVec3::new(-2000.0, -2000.0, 0.0), DVec3::X * 4000.0, DVec3::Y * 4000.0, 80);
        // the part: 40 x 20 x 10 at the middle
        add_box(&mut m, -10.0, -5.0, 30.0, 15.0, 10.0, 20);
        // a 25 x 50 x 75 box lying flat beside it
        add_box(&mut m, -120.0, -60.0, -45.0, -10.0, 25.0, 12);
        // furniture: a sofa-sized block 1.5 m away, and a wall
        add_box(&mut m, 1200.0, -600.0, 2000.0, 600.0, 450.0, 10);
        quad(&mut m, DVec3::new(-2000.0, 1900.0, 0.0), DVec3::X * 4000.0, DVec3::Z * 2500.0, 30);
        // cameras 400 mm out and 300 mm up, looking at the part
        let target = DVec3::new(10.0, 5.0, 5.0);
        let cams: Vec<(DVec3, DVec3)> = (0..24)
            .map(|i| {
                let a = i as f64 / 24.0 * std::f64::consts::TAU;
                let c = DVec3::new(a.cos() * 400.0, a.sin() * 400.0, 300.0);
                (c, (target - c).normalize())
            })
            .collect();
        let m = weld(&m);
        let c = clean(&m, &Cleanup { reference: Some([25.0, 50.0, 75.0]), triangles: 0, ..Default::default() }, &cams);
        let s = c.scale.expect("found the box");
        assert!((s - 1.0).abs() < 0.02, "{s} {:?}", c.notes);
        let (lo, hi) = c.mesh.bbox();
        let size = (hi - lo) * s;
        assert!((size.x - 40.0).abs() < 1.0 && (size.y - 20.0).abs() < 1.0 && (size.z - 10.0).abs() < 1.0, "{size:?} {:?}", c.notes);
    }

    #[test]
    fn reference_fit_trusts_the_footprint_on_a_soft_floor() {
        // a 25 x 50 x 75 box lying flat, its height read 3 mm short (sunk into carpet)
        let (s, spread, flat) = reference_fit([75.0, 50.0], 22.0, [25.0, 50.0, 75.0]);
        assert!(flat && (s - 1.0).abs() < 1e-9 && spread < 1e-9);
        // standing on end
        let (s, spread, flat) = reference_fit([50.0, 25.0], 75.0, [25.0, 50.0, 75.0]);
        assert!(!flat && (s - 1.0).abs() < 1e-9 && spread < 1e-9);
    }

    #[test]
    fn finds_table_levels_and_cuts() {
        let (m, r, _) = scene();
        let pl = find_table(&m, 0.01, None).unwrap();
        assert!(pl.n.dot(r * DVec3::Z) > 0.9999, "{:?}", pl.n);
        let c = clean(&m, &Cleanup { level: true, remove_table: true, triangles: 0, reference: None }, &[]);
        let (lo, hi) = c.mesh.bbox();
        let size = (hi - lo) * 50.0;
        // the box's sides start just above the cut; long side along X, centred on the origin
        assert!((size.x - 40.0).abs() < 0.1 && (size.y - 20.0).abs() < 0.1, "{size:?}");
        assert!((hi.z * 50.0 - 10.0).abs() < 0.01, "{hi:?}");
        assert!(((lo.x + hi.x) / 2.0).abs() < 1e-6 && ((lo.y + hi.y) / 2.0).abs() < 1e-6);
    }

    #[test]
    fn decimates_keeping_shape() {
        let (m, _, _) = scene();
        let c = clean(&m, &Cleanup { level: true, remove_table: false, triangles: 0, reference: None }, &[]);
        let full = c.mesh;
        let d = decimate(&full, 2000);
        assert!(d.t.len() <= 2000 && d.t.len() > 1500, "{}", d.t.len());
        let (lo, hi) = full.bbox();
        let (lo2, hi2) = d.bbox();
        assert!((lo - lo2).length() < 1e-6 && (hi - hi2).length() < 1e-6, "{lo:?} {hi:?} {lo2:?} {hi2:?}");
        // the box top keeps its height: some vertex still at z = 10/50
        assert!(d.v.iter().any(|p| (p.z - 0.2).abs() < 1e-6));
    }

    #[test]
    fn ply_round_trip_and_ascii() {
        let (m, _, _) = scene();
        let b = write_ply(&m);
        let r = read_ply(&b).unwrap();
        assert_eq!(r.t.len(), m.t.len());
        assert!((r.v[5] - m.v[5]).length() < 1e-5);
        let a = "ply\nformat ascii 1.0\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 1\n1 0 0 2\n1 1 0 3\n0 1 0 4\n4 0 1 2 3\n";
        let r = read_ply(a.as_bytes()).unwrap();
        assert_eq!(r.t, vec![[0, 1, 2], [0, 2, 3]]);
        assert!(read_ply(b"ply\nformat ascii 1.0\nelement vertex 1\nproperty float x\nproperty float y\nproperty float z\nend_header\n1 2 3\n").is_err());
    }

    #[test]
    fn scale_from_two_clicks_on_the_table() {
        let (m, r, o) = scene();
        let pl = find_table(&m, 0.01, None);
        // a camera above the table looking straight down: world (model) up is r*Z
        let up = r * DVec3::Z;
        let centre = o + up * 4.0 + r * DVec3::new(0.0, -1.0, 0.0);
        // camera axes: z (view) = -up, x = r*X, y = z × x
        let zc = -up;
        let xc = r * DVec3::X;
        let yc = zc.cross(xc);
        let rot = DMat3::from_cols(xc, yc, zc).transpose();
        let cam = Camera { name: "a".into(), r: rot, t: -(rot * centre), k: [1000.0, 1000.0, 500.0, 500.0] };
        // two table points 50 mm apart along X, projected into the photo
        let proj = |p: DVec3| {
            let c = rot * p + cam.t;
            [c.x / c.z * 1000.0 + 500.0, c.y / c.z * 1000.0 + 500.0]
        };
        let pa = o + r * DVec3::new(-60.0, -60.0, 0.0) / 50.0;
        let pb = o + r * DVec3::new(-10.0, -60.0, 0.0) / 50.0;
        let s = scale_from_clicks(&m, pl, &cam, proj(pa), proj(pb), 50.0).unwrap();
        assert!((s - 50.0).abs() < 0.01, "{s}");
    }
}
