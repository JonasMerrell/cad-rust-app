//! Periodic boundary conditions (not in crackh3d): the nodes of a set of `follower` faces move with the
//! matching points of a set of `leader` faces, `u(x) = u(x - offset) + jump`. The two sides need not be
//! meshed alike: a follower node follows the leader surface at `x - offset`, interpolated with the leader
//! facet's shape functions, so its displacement is a fixed combination of the leader facet's nodes
//! (`fe::bcs` writes it into the master-slave matrix `T`).

use std::collections::{BTreeMap, HashMap};

use super::bcs::{facet_shape, PeriodicSet, SurfFacet};
use super::solve::SolveError;

type V3 = [f64; 3];

/// How a follower node moves: `u = sum w_j u_j + jump`, with the BC it comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Tie {
    pub leaders: Vec<(usize, f64)>,
    pub jump: V3,
    pub owner: usize,
}

/// Natural coordinates of a facet's corners.
fn corners_local(f: &SurfFacet) -> &'static [[f64; 2]] {
    match f {
        SurfFacet::Tri6(_) => &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        SurfFacet::Quad8(_) => &[[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]],
    }
}

fn clamp_local(f: &SurfFacet, p: [f64; 2]) -> [f64; 2] {
    match f {
        SurfFacet::Tri6(_) => {
            let (mut a, mut b) = (p[0].max(0.0), p[1].max(0.0));
            let s = a + b;
            if s > 1.0 {
                a /= s;
                b /= s;
            }
            [a, b]
        }
        SurfFacet::Quad8(_) => [p[0].clamp(-1.0, 1.0), p[1].clamp(-1.0, 1.0)],
    }
}

/// The point of facet `f` closest to `p`: its natural coordinates, shape function values and distance
/// (Gauss-Newton from the centre, kept inside the facet).
fn closest_on_facet(pts: &[V3], f: &SurfFacet, p: V3) -> ([f64; 2], Vec<f64>, f64) {
    let c = corners_local(f);
    let mut s = [c.iter().map(|x| x[0]).sum::<f64>() / c.len() as f64, c.iter().map(|x| x[1]).sum::<f64>() / c.len() as f64];
    let eval = |s: [f64; 2]| {
        let (n, d) = facet_shape(f, s);
        let mut x = [0.0; 3];
        let mut t = [[0.0; 3]; 2];
        for (a, &id) in f.nodes().iter().enumerate() {
            for k in 0..3 {
                x[k] += n[a] * pts[id][k];
                t[0][k] += d[a][0] * pts[id][k];
                t[1][k] += d[a][1] * pts[id][k];
            }
        }
        (n, x, t)
    };
    for _ in 0..12 {
        let (_, x, t) = eval(s);
        let r = [p[0] - x[0], p[1] - x[1], p[2] - x[2]];
        let dot = |a: V3, b: V3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let (a, b, cc) = (dot(t[0], t[0]), dot(t[0], t[1]), dot(t[1], t[1]));
        let det = a * cc - b * b;
        if det.abs() < 1e-300 {
            break;
        }
        let (g0, g1) = (dot(t[0], r), dot(t[1], r));
        let ds = [(cc * g0 - b * g1) / det, (a * g1 - b * g0) / det];
        let next = clamp_local(f, [s[0] + ds[0], s[1] + ds[1]]);
        let moved = (next[0] - s[0]).abs() + (next[1] - s[1]).abs();
        s = next;
        if moved < 1e-12 {
            break;
        }
    }
    let (n, x, _) = eval(s);
    let d = ((p[0] - x[0]).powi(2) + (p[1] - x[1]).powi(2) + (p[2] - x[2]).powi(2)).sqrt();
    (s, n, d)
}

fn bbox(pts: &[V3], f: &SurfFacet) -> (V3, V3) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for &id in f.nodes() {
        for k in 0..3 {
            lo[k] = lo[k].min(pts[id][k]);
            hi[k] = hi[k].max(pts[id][k]);
        }
    }
    (lo, hi)
}

/// The tie of every follower node of `sets` (BC numbers from `first_owner` on). A node on the follower
/// faces of two sets (an edge of a unit cell periodic in two directions) follows the first; its leader may
/// itself be a follower, which `fe::bcs` resolves in turn.
pub fn ties(pts: &[V3], sets: &[PeriodicSet], first_owner: usize) -> Result<BTreeMap<usize, Tie>, SolveError> {
    let mut out: BTreeMap<usize, Tie> = BTreeMap::new();
    for (si, s) in sets.iter().enumerate() {
        if s.leader.is_empty() || s.follower.is_empty() {
            return Err(SolveError::Invalid(format!("{}: a periodic BC needs faces on both sides", s.name)));
        }
        if s.offset.iter().all(|x| *x == 0.0) {
            return Err(SolveError::Invalid(format!("{}: the offset between the two sides is zero", s.name)));
        }
        // leader facets binned by their (padded) boxes
        let sizes: Vec<f64> = s
            .leader
            .iter()
            .map(|f| {
                let (lo, hi) = bbox(pts, f);
                (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max)
            })
            .collect();
        let h = (sizes.iter().sum::<f64>() / sizes.len() as f64).max(1e-12);
        let cell = 2.0 * h;
        let key = |p: V3| [(p[0] / cell).floor() as i64, (p[1] / cell).floor() as i64, (p[2] / cell).floor() as i64];
        let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
        for (fi, f) in s.leader.iter().enumerate() {
            let (lo, hi) = bbox(pts, f);
            let pad = 0.25 * sizes[fi];
            let (a, b) = (key(lo.map(|x| x - pad)), key(hi.map(|x| x + pad)));
            for x in a[0]..=b[0] {
                for y in a[1]..=b[1] {
                    for z in a[2]..=b[2] {
                        grid.entry([x, y, z]).or_default().push(fi);
                    }
                }
            }
        }
        let mut nodes: Vec<usize> = s.follower.iter().flat_map(|f| f.nodes().iter().copied()).collect();
        nodes.sort_unstable();
        nodes.dedup();
        for n in nodes {
            if out.contains_key(&n) {
                continue;
            }
            let p = [pts[n][0] - s.offset[0], pts[n][1] - s.offset[1], pts[n][2] - s.offset[2]];
            let k = key(p);
            let mut best: Option<(f64, usize, Vec<f64>)> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &fi in grid.get(&[k[0] + dx, k[1] + dy, k[2] + dz]).map(|v| v.as_slice()).unwrap_or(&[]) {
                            let (_, w, d) = closest_on_facet(pts, &s.leader[fi], p);
                            if best.as_ref().is_none_or(|b| d < b.0) {
                                best = Some((d, fi, w));
                            }
                        }
                    }
                }
            }
            let Some((d, fi, w)) = best.filter(|b| b.0 <= 0.25 * sizes[b.1].max(h * 0.1)) else {
                return Err(SolveError::Invalid(format!(
                    "{}: the node at ({:.4}, {:.4}, {:.4}) has no match on the other side {:.4} away: the two sides are not the same shape moved by the offset",
                    s.name, pts[n][0], pts[n][1], pts[n][2], (s.offset[0].powi(2) + s.offset[1].powi(2) + s.offset[2].powi(2)).sqrt()
                )));
            };
            let _ = d;
            // weights a hair from zero (a point on a facet's edge, found to round-off) are dropped, so the
            // node follows that edge's nodes only
            let mut leaders: Vec<(usize, f64)> = s.leader[fi].nodes().iter().copied().zip(w).filter(|x| x.1.abs() > 1e-9).collect();
            let sum: f64 = leaders.iter().map(|x| x.1).sum();
            leaders.iter_mut().for_each(|x| x.1 /= sum);
            if leaders.iter().any(|x| x.0 == n) {
                return Err(SolveError::Invalid(format!("{}: the two sides overlap (a node would follow itself)", s.name)));
            }
            out.insert(n, Tie { leaders, jump: s.jump, owner: first_owner + si });
        }
    }
    Ok(out)
}
