//! Port of `crackh3d/elements.py`: reference metadata for tet4 / tet10 /
//! hex20 / wedge15 / pyramid13, in gmsh's own local node ordering, with
//! midside parent corners and element faces derived from the coordinates.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Port of `elements.GMSH_TRI6` / `GMSH_QUAD8`: gmsh type numbers of the face types.
pub const GMSH_TRI6: i32 = 9;
pub const GMSH_QUAD8: i32 = 16;

/// Port of `elements.QUADRATIC_SOLID_TYPES`.
pub const QUADRATIC_SOLID_TYPES: [&str; 4] = ["tet10", "wedge15", "hex20", "pyramid13"];

/// Port of `elements.ElementType`.
#[derive(Debug, Clone)]
pub struct ElementType {
    pub name: &'static str,
    pub gmsh_type: i32,
    pub num_nodes: usize,
    pub num_corners: usize,
    /// Reference coordinate of each local node (`num_nodes` rows).
    pub local_coords: Vec<[f64; 3]>,
    pub corner_indices: Vec<usize>,
    pub midside_indices: Vec<usize>,
    /// For each midside local index, the pair of corner local indices it lies between.
    pub midside_parents: BTreeMap<usize, (usize, usize)>,
}

type Raw = (&'static str, i32, usize, Vec<[f64; 3]>);

#[rustfmt::skip]
fn raw() -> Vec<Raw> {
    let p = |v: &[(f64, f64, f64)]| -> Vec<[f64; 3]> { v.iter().map(|&(a, b, c)| [a, b, c]).collect() };
    vec![
        ("tet4", 4, 4, p(&[(0., 0., 0.), (1., 0., 0.), (0., 1., 0.), (0., 0., 1.)])),
        ("tet10", 11, 4, p(&[
            (0., 0., 0.), (1., 0., 0.), (0., 1., 0.), (0., 0., 1.),
            (0.5, 0., 0.), (0.5, 0.5, 0.), (0., 0.5, 0.),
            (0., 0., 0.5), (0., 0.5, 0.5), (0.5, 0., 0.5),
        ])),
        ("hex20", 17, 8, p(&[
            (-1., -1., -1.), (1., -1., -1.), (1., 1., -1.), (-1., 1., -1.),
            (-1., -1., 1.), (1., -1., 1.), (1., 1., 1.), (-1., 1., 1.),
            (0., -1., -1.), (-1., 0., -1.), (-1., -1., 0.), (1., 0., -1.),
            (1., -1., 0.), (0., 1., -1.), (1., 1., 0.), (-1., 1., 0.),
            (0., -1., 1.), (-1., 0., 1.), (1., 0., 1.), (0., 1., 1.),
        ])),
        ("wedge15", 18, 6, p(&[
            (0., 0., -1.), (1., 0., -1.), (0., 1., -1.),
            (0., 0., 1.), (1., 0., 1.), (0., 1., 1.),
            (0.5, 0., -1.), (0., 0.5, -1.), (0., 0., 0.),
            (0.5, 0.5, -1.), (1., 0., 0.), (0., 1., 0.),
            (0.5, 0., 1.), (0., 0.5, 1.), (0.5, 0.5, 1.),
        ])),
        ("pyramid13", 19, 5, p(&[
            (-1., -1., 0.), (1., -1., 0.), (1., 1., 0.), (-1., 1., 0.), (0., 0., 1.),
            (0., -1., 0.), (-1., 0., 0.), (-0.5, -0.5, 0.5), (1., 0., 0.),
            (0.5, -0.5, 0.5), (0., 1., 0.), (0.5, 0.5, 0.5), (-0.5, 0.5, 0.5),
        ])),
    ]
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Port of `elements._build`. Panics (like the Python `RuntimeError`) if a
/// midside node is not on a corner-corner midpoint.
fn build(r: &Raw) -> ElementType {
    let (name, gmsh_type, num_corners, ref coords) = *r;
    let coords = coords.clone();
    let num_nodes = coords.len();
    let mut midside_parents = BTreeMap::new();
    for m in num_corners..num_nodes {
        let mut best: Option<(f64, usize, usize)> = None;
        for i in 0..num_corners {
            for j in (i + 1)..num_corners {
                let mid = [
                    0.5 * (coords[i][0] + coords[j][0]),
                    0.5 * (coords[i][1] + coords[j][1]),
                    0.5 * (coords[i][2] + coords[j][2]),
                ];
                let d = norm(sub(mid, coords[m]));
                if best.map_or(true, |b| d < b.0) {
                    best = Some((d, i, j));
                }
            }
        }
        let (d, i, j) = best.unwrap();
        assert!(d <= 1e-8, "{name}: midside node {m} does not lie on any corner-corner midpoint (closest residual {d})");
        midside_parents.insert(m, (i, j));
    }
    ElementType {
        name,
        gmsh_type,
        num_nodes,
        num_corners,
        local_coords: coords,
        corner_indices: (0..num_corners).collect(),
        midside_indices: (num_corners..num_nodes).collect(),
        midside_parents,
    }
}

/// Port of `elements.ELEMENT_TYPES` (insertion order tet4, tet10, hex20, wedge15, pyramid13).
pub fn element_types() -> &'static [ElementType] {
    static T: OnceLock<Vec<ElementType>> = OnceLock::new();
    T.get_or_init(|| raw().iter().map(build).collect())
}

/// `ELEMENT_TYPES[name]`.
pub fn element_type(name: &str) -> Option<&'static ElementType> {
    element_types().iter().find(|e| e.name == name)
}

/// Port of `elements.GMSH_TYPE_TO_NAME`.
pub fn gmsh_type_to_name(gmsh_type: i32) -> Option<&'static str> {
    element_types().iter().find(|e| e.gmsh_type == gmsh_type).map(|e| e.name)
}

/// Port of `elements.NAME_TO_GMSH_TYPE`.
pub fn name_to_gmsh_type(name: &str) -> Option<i32> {
    element_type(name).map(|e| e.gmsh_type)
}

/// One element face: local node indices (corners wound about the face
/// centroid, then midsides) and the gmsh face type (`GMSH_TRI6`/`GMSH_QUAD8`).
pub type Face = (Vec<usize>, i32);

/// Port of `elements._face_nodes`. `None` where the Python raises `KeyError`
/// (tet4 has no midside nodes to complete its faces).
fn compute_faces(et: &ElementType) -> Option<Vec<Face>> {
    let c = &et.local_coords;
    let corners = &et.corner_indices;
    let nc = corners.len();

    // (corner ids on the plane, outward normal)
    let mut faces: Vec<(Vec<usize>, [f64; 3])> = Vec::new();
    let mut seen: Vec<Vec<usize>> = Vec::new();
    for i in 0..nc {
        for j in (i + 1)..nc {
            for k in (j + 1)..nc {
                let (p0, p1, p2) = (c[corners[i]], c[corners[j]], c[corners[k]]);
                let n = cross(sub(p1, p0), sub(p2, p0));
                let nn = norm(n);
                if nn < 1e-12 {
                    continue;
                }
                let mut n = [n[0] / nn, n[1] / nn, n[2] / nn];
                let d: Vec<f64> = corners.iter().map(|&q| dot(sub(c[q], p0), n)).collect();
                let on: Vec<bool> = d.iter().map(|x| x.abs() < 1e-9).collect();
                if d.iter().any(|&x| x > 1e-9) && d.iter().any(|&x| x < -1e-9) {
                    continue; // cuts through, not a face
                }
                let mut key: Vec<usize> = corners.iter().zip(&on).filter(|(_, &o)| o).map(|(&q, _)| q).collect();
                let ids = key.clone();
                key.sort_unstable();
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                if d.iter().any(|&x| x > 1e-9) {
                    n = [-n[0], -n[1], -n[2]];
                }
                faces.push((ids, n));
            }
        }
    }

    let mut out = Vec::new();
    for (ids, n) in faces {
        let m = ids.len() as f64;
        let mut mid = [0.0; 3];
        for &q in &ids {
            for a in 0..3 {
                mid[a] += c[q][a];
            }
        }
        // numpy mean: sum / count
        let mid = [mid[0] / m, mid[1] / m, mid[2] / m];
        let e1 = sub(c[ids[0]], mid);
        let ne1 = norm(e1);
        let e1 = [e1[0] / ne1, e1[1] / ne1, e1[2] / ne1];
        let e2 = cross(n, e1);
        let mut ang: Vec<(f64, usize)> = ids
            .iter()
            .map(|&q| {
                let r = sub(c[q], mid);
                (dot(r, e2).atan2(dot(r, e1)), q)
            })
            .collect();
        ang.sort_by(|a, b| a.0.total_cmp(&b.0));
        let ring: Vec<usize> = ang.iter().map(|x| x.1).collect();
        let mut nodes = ring.clone();
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            let m = et
                .midside_parents
                .iter()
                .find(|(_, &(p, q))| (p == a && q == b) || (p == b && q == a))
                .map(|(&m, _)| m)?;
            nodes.push(m);
        }
        let ty = if ring.len() == 3 { GMSH_TRI6 } else { GMSH_QUAD8 };
        out.push((nodes, ty));
    }
    Some(out)
}

/// Port of `elements.face_nodes`: `[(node indices, gmsh face type), ...]` for
/// one element's faces, cached per element type. `None` for tet4 (the Python
/// raises `KeyError`) and for unknown names.
pub fn face_nodes(element_type_name: &str) -> Option<&'static [Face]> {
    static CACHE: OnceLock<Vec<Option<Vec<Face>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| element_types().iter().map(compute_faces).collect());
    let idx = element_types().iter().position(|e| e.name == element_type_name)?;
    cache[idx].as_deref()
}
