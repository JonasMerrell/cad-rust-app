//! Importing parts from other programs: STEP, Parasolid and IGES (exact solids) and
//! STL and OBJ (triangle meshes, turned into solids). The file is stored inside
//! the document, compressed, so a saved part never depends on the original file.

use std::io::{Read, Write};

use base64::Engine;

/// File format from a path's extension.
pub fn format_of(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "step" | "stp" => Some("step"),
        "x_t" | "xmt_txt" => Some("parasolid"),
        "iges" | "igs" => Some("iges"),
        "stl" => Some("stl"),
        "obj" => Some("obj"),
        _ => None,
    }
}

/// The format and bytes to store for an imported file: Parasolid is turned into STEP here.
pub fn storable(format: &'static str, bytes: Vec<u8>, name: &str) -> Result<(&'static str, Vec<u8>), String> {
    if format == "parasolid" {
        let c = crate::parasolid::to_step(&bytes, name)?;
        if c.skipped > 0 {
            eprintln!("{name}: {} parts from Parasolid, {} faces left out (geometry rCAD can't convert yet)", c.parts.len(), c.skipped);
        }
        return Ok(("step", c.step.into_bytes()));
    }
    Ok((format, bytes))
}

/// Compress and encode file bytes for storing in the document.
pub fn pack(bytes: &[u8]) -> String {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = z.write_all(bytes);
    base64::engine::general_purpose::STANDARD.encode(z.finish().unwrap_or_default())
}

pub fn unpack(data: &str) -> Result<Vec<u8>, String> {
    let z = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| format!("the stored file is damaged: {e}"))?;
    let mut out = vec![];
    flate2::read::ZlibDecoder::new(&z[..]).read_to_end(&mut out).map_err(|e| format!("the stored file is damaged: {e}"))?;
    Ok(out)
}

/// A triangle mesh with shared vertices.
#[derive(Default, Debug)]
pub struct Mesh {
    pub pts: Vec<f64>,
    pub tris: Vec<u32>,
}

impl Mesh {
    fn from_triangles(tris: &[[[f64; 3]; 3]], scale: f64) -> Mesh {
        // weld vertices closer than a micron (scaled)
        let q = 1e-6_f64.max(1e-9);
        let mut map: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
        let mut m = Mesh::default();
        for t in tris {
            let mut ids = [0u32; 3];
            for (k, p) in t.iter().enumerate() {
                let p = [p[0] * scale, p[1] * scale, p[2] * scale];
                let key = p.map(|v| (v / q).round() as i64);
                ids[k] = *map.entry(key).or_insert_with(|| {
                    m.pts.extend(p);
                    (m.pts.len() / 3 - 1) as u32
                });
            }
            if ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2] {
                m.tris.extend(ids);
            }
        }
        m
    }

    pub fn triangles(&self) -> usize {
        self.tris.len() / 3
    }
}

/// Read an STL (binary or ASCII) or OBJ file into a welded mesh, scaled.
pub fn read_mesh(format: &str, bytes: &[u8], scale: f64) -> Result<Mesh, String> {
    let tris = match format {
        "stl" => read_stl(bytes)?,
        "obj" => read_obj(bytes)?,
        _ => return Err(format!("{format} is not a mesh format")),
    };
    if tris.is_empty() {
        return Err("the file has no triangles".into());
    }
    Ok(Mesh::from_triangles(&tris, scale))
}

fn read_stl(b: &[u8]) -> Result<Vec<[[f64; 3]; 3]>, String> {
    // binary STL: 80-byte header, count, 50 bytes per triangle (some binary files start with "solid" too)
    if b.len() >= 84 {
        let n = u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as usize;
        if 84 + n * 50 == b.len() {
            let f = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as f64;
            return Ok((0..n)
                .map(|i| {
                    let o = 84 + i * 50 + 12;
                    [0, 1, 2].map(|k| [f(o + k * 12), f(o + k * 12 + 4), f(o + k * 12 + 8)])
                })
                .collect());
        }
    }
    let text = String::from_utf8_lossy(b);
    if !text.trim_start().starts_with("solid") {
        return Err("this is not an STL file".into());
    }
    let mut verts = vec![];
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some("vertex") {
            let v: Vec<f64> = it.take(3).filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                verts.push([v[0], v[1], v[2]]);
            }
        }
    }
    Ok(verts.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

fn read_obj(b: &[u8]) -> Result<Vec<[[f64; 3]; 3]>, String> {
    let text = String::from_utf8_lossy(b);
    let mut v: Vec<[f64; 3]> = vec![];
    let mut tris = vec![];
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let p: Vec<f64> = it.take(3).filter_map(|x| x.parse().ok()).collect();
                if p.len() == 3 {
                    v.push([p[0], p[1], p[2]]);
                }
            }
            Some("f") => {
                // "f 1 2 3", "f 1/1/1 ...", negative indices count from the end; polygons fan out
                let idx: Vec<usize> = it
                    .filter_map(|w| w.split('/').next()?.parse::<i64>().ok())
                    .filter_map(|i| if i > 0 { Some(i as usize - 1) } else if i < 0 { v.len().checked_sub((-i) as usize) } else { None })
                    .collect();
                for k in 1..idx.len().saturating_sub(1) {
                    if let (Some(a), Some(b), Some(c)) = (v.get(idx[0]), v.get(idx[k]), v.get(idx[k + 1])) {
                        tris.push([*a, *b, *c]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(tris)
}

/// Faces for the B-rep builder (see `ex_mesh_solid`): each triangle on its own, or,
/// with `merge`, connected coplanar triangles joined into polygons with holes.
pub fn face_spec(pts: &[f64], tris: &[u32], merge: bool) -> Vec<u32> {
    use glam::DVec3;
    let p = |i: u32| DVec3::new(pts[3 * i as usize], pts[3 * i as usize + 1], pts[3 * i as usize + 2]);
    let nt = tris.len() / 3;
    let tri = |t: usize| [tris[3 * t], tris[3 * t + 1], tris[3 * t + 2]];
    let normal = |t: usize| {
        let [a, b, c] = tri(t);
        (p(b) - p(a)).cross(p(c) - p(a)).normalize_or_zero()
    };
    let mut out = vec![];
    let single = |out: &mut Vec<u32>, t: usize| {
        let [a, b, c] = tri(t);
        out.extend([1, 3, a, b, c, 1, a, b, c]);
    };
    if !merge {
        (0..nt).for_each(|t| single(&mut out, t));
        return out;
    }
    // size for tolerances
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for i in 0..pts.len() / 3 {
        lo = lo.min(p(i as u32));
        hi = hi.max(p(i as u32));
    }
    // STL coordinates are 32-bit floats: allow for their rounding
    let tol = (hi - lo).length().max(1.0) * 1e-5;
    // triangles sharing an edge, by undirected edge
    let mut by_edge: std::collections::HashMap<(u32, u32), Vec<usize>> = std::collections::HashMap::new();
    for t in 0..nt {
        let v = tri(t);
        for k in 0..3 {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(t);
        }
    }
    // union-find over coplanar neighbours
    let mut parent: Vec<usize> = (0..nt).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let normals: Vec<DVec3> = (0..nt).map(normal).collect();
    for ts in by_edge.values() {
        if ts.len() != 2 {
            continue;
        }
        let (t, u) = (ts[0], ts[1]);
        let (n1, n2) = (normals[t], normals[u]);
        if n1 == DVec3::ZERO || n2 == DVec3::ZERO || n1.dot(n2) < 1.0 - 1e-6 {
            continue;
        }
        // the other triangle's vertices lie on this one's plane
        let a = p(tri(t)[0]);
        if tri(u).iter().all(|&v| (p(v) - a).dot(n1).abs() < tol) {
            let (x, y) = (find(&mut parent, t), find(&mut parent, u));
            parent[x] = y;
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for t in 0..nt {
        let r = find(&mut parent, t);
        groups.entry(r).or_default().push(t);
    }
    let mut roots: Vec<usize> = groups.keys().copied().collect();
    roots.sort_unstable();
    for r in roots {
        let ts = &groups[&r];
        if ts.len() == 1 {
            single(&mut out, ts[0]);
            continue;
        }
        // boundary: directed edges whose reverse is not in the group
        let mut directed: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        for &t in ts {
            let v = tri(t);
            for k in 0..3 {
                directed.insert((v[k], v[(k + 1) % 3]));
            }
        }
        let mut next: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
        let mut nb = 0;
        for &(a, b) in &directed {
            if !directed.contains(&(b, a)) {
                next.entry(a).or_default().push(b);
                nb += 1;
            }
        }
        // chain into loops; give up on this group if the boundary is not simple
        let simple = next.values().all(|v| v.len() == 1);
        let mut loops: Vec<Vec<u32>> = vec![];
        if simple {
            let mut used = 0;
            let mut starts: Vec<u32> = next.keys().copied().collect();
            starts.sort_unstable();
            let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for s0 in starts {
                if seen.contains(&s0) {
                    continue;
                }
                let mut l = vec![s0];
                seen.insert(s0);
                let mut cur = next[&s0][0];
                while cur != s0 {
                    if !seen.insert(cur) || l.len() > nb {
                        break;
                    }
                    l.push(cur);
                    match next.get(&cur) {
                        Some(v) => cur = v[0],
                        None => break,
                    }
                }
                used += l.len();
                loops.push(l);
            }
            if used != nb {
                loops.clear();
            }
        }
        let n = normals[ts[0]];
        let area = |l: &[u32]| -> f64 {
            let mut s = DVec3::ZERO;
            for k in 0..l.len() {
                s += p(l[k]).cross(p(l[(k + 1) % l.len()]));
            }
            s.dot(n) / 2.0
        };
        let outer = loops.iter().enumerate().filter(|(_, l)| area(l) > 0.0).max_by(|a, b| area(a.1).total_cmp(&area(b.1))).map(|(i, _)| i);
        let positive = loops.iter().filter(|l| area(l) > 0.0).count();
        match outer {
            Some(o) if positive == 1 && loops.iter().all(|l| l.len() >= 3) => {
                out.push(loops.len() as u32);
                let order = std::iter::once(o).chain((0..loops.len()).filter(|&i| i != o));
                for i in order {
                    out.push(loops[i].len() as u32);
                    out.extend(&loops[i]);
                }
                out.push(ts.len() as u32);
                for &t in ts {
                    out.extend(tri(t));
                }
            }
            _ => ts.iter().for_each(|&t| single(&mut out, t)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_stl() -> String {
        // 12 triangles of a unit cube, outward
        let q = [
            ([0., 0., 0.], [0., 1., 0.], [1., 1., 0.], [1., 0., 0.]),
            ([0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]),
            ([0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]),
            ([0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]),
            ([0., 0., 0.], [0., 0., 1.], [0., 1., 1.], [0., 1., 0.]),
            ([1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]),
        ];
        let mut s = String::from("solid cube\n");
        let v = |p: [f64; 3]| format!("vertex {} {} {}\n", p[0], p[1], p[2]);
        for (a, b, c, d) in q {
            for t in [[a, b, c], [a, c, d]] {
                s += "facet normal 0 0 0\nouter loop\n";
                for p in t {
                    s += &v(p);
                }
                s += "endloop\nendfacet\n";
            }
        }
        s + "endsolid cube\n"
    }

    #[test]
    fn stl_and_obj_meshes_weld_and_pack() {
        let m = read_mesh("stl", cube_stl().as_bytes(), 10.0).unwrap();
        assert_eq!(m.pts.len() / 3, 8);
        assert_eq!(m.triangles(), 12);
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
        assert_eq!(read_mesh("obj", obj.as_bytes(), 1.0).unwrap().triangles(), 2);
        let data = cube_stl();
        assert_eq!(unpack(&pack(data.as_bytes())).unwrap(), data.as_bytes());
    }

    #[test]
    fn mesh_becomes_a_solid() {
        let m = read_mesh("stl", cube_stl().as_bytes(), 10.0).unwrap();
        let s = crate::occ::mesh_solid(&m.pts, &m.tris, true).unwrap();
        let v = crate::kernel::volume(&s);
        assert!((v - 1000.0).abs() < 1e-6, "{v}");
        // coplanar triangles merge into the 6 faces of a box
        assert_eq!(crate::kernel::unique_faces(&s).len(), 6);
    }
}

#[cfg(test)]
mod timing {
    #[test]
    #[ignore]
    fn time_import() {
        let path = std::env::var("PROBE").unwrap();
        let t = std::time::Instant::now();
        let bytes = std::fs::read(&path).unwrap();
        let m = super::read_mesh("stl", &bytes, 1.0).unwrap();
        eprintln!("parse {:?} tris {}", t.elapsed(), m.triangles());
        let t = std::time::Instant::now();
        let s = crate::occ::mesh_solid(&m.pts, &m.tris, true).unwrap();
        eprintln!("brep {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let b = crate::kernel::mesh_body(&s).unwrap();
        eprintln!("mesh_body {:?} faces {}", t.elapsed(), b.faces.len());
    }
}
