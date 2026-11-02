//! A grid autorouter for PCB tabs: A* on two layers with vias, one airwire at a time, shortest
//! first. Copper of other nets is kept out by its clearance; zones are refilled afterwards and
//! anything they no longer connect is routed in another pass.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::pcb::{self, Cu, Item, Layer, Pcb, Track, Via, WPad};
use crate::pcb_lib::P;

const FREE: i32 = -1;
const WALL: i32 = -2;

/// Per layer, which cells something of half-width `h` that keeps `c` from other nets may use:
/// FREE, WALL, or only one net's. There is a map for each track width and via size the net
/// classes use.
struct Map {
    h: f64,
    c: f64,
    via: bool,
    m: [Vec<i32>; 2],
}

struct Grid {
    lo: P,
    g: f64,
    nx: usize,
    ny: usize,
    maps: Vec<Map>,
}

fn li(l: Layer) -> usize {
    match l {
        Layer::Top => 0,
        Layer::Bottom => 1,
    }
}

impl Grid {
    fn centre(&self, x: usize, y: usize) -> P {
        [self.lo[0] + (x as f64 + 0.5) * self.g, self.lo[1] + (y as f64 + 0.5) * self.g]
    }
    fn cell(&self, p: P) -> Option<(usize, usize)> {
        let x = ((p[0] - self.lo[0]) / self.g).floor();
        let y = ((p[1] - self.lo[1]) / self.g).floor();
        (x >= 0.0 && y >= 0.0 && (x as usize) < self.nx && (y as usize) < self.ny).then_some((x as usize, y as usize))
    }

    /// Mark the cells within each map's half-width plus the clearance (the larger of the copper's
    /// and the map's; none for a keepout) of a piece of copper.
    fn mark(&mut self, cu: &Cu, layer: Option<Layer>, net: i32, clearance: f64) {
        let all: Vec<usize> = (0..self.maps.len()).collect();
        self.mark_maps(&all, cu, layer, net, Some(clearance));
    }

    fn mark_maps(&mut self, maps: &[usize], cu: &Cu, layer: Option<Layer>, net: i32, clearance: Option<f64>) {
        let (lo, hi) = cu.bbox();
        for &mi in maps {
            let c = clearance.map(|c| c.max(self.maps[mi].c)).unwrap_or(0.0);
            let r = c + self.maps[mi].h + self.g * 0.5;
            let x0 = (((lo[0] - r - self.lo[0]) / self.g).floor().max(0.0)) as usize;
            let y0 = (((lo[1] - r - self.lo[1]) / self.g).floor().max(0.0)) as usize;
            let x1 = (((hi[0] + r - self.lo[0]) / self.g).ceil() as usize).min(self.nx);
            let y1 = (((hi[1] + r - self.lo[1]) / self.g).ceil() as usize).min(self.ny);
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = self.centre(x, y);
                    let gap = match cu {
                        Cu::Seg { a, b, r } => pcb::seg_point(*a, *b, c) - r,
                        Cu::Poly(p) => {
                            if pcb::inside(p, c) {
                                0.0
                            } else {
                                pcb::outline_dist(p, c)
                            }
                        }
                    };
                    if gap >= r {
                        continue;
                    }
                    for l in [Layer::Top, Layer::Bottom] {
                        if layer.is_some_and(|x| x != l) {
                            continue;
                        }
                        let v = &mut self.maps[mi].m[li(l)][y * self.nx + x];
                        *v = if net >= 0 && (*v == FREE || *v == net) { net } else { WALL };
                    }
                }
            }
        }
    }
}

fn free(v: i32, net: i32) -> bool {
    v == FREE || v == net
}

pub struct Outcome {
    pub routed: usize,
    pub tracks: usize,
    pub vias: usize,
    /// Connections that found no way through, as net names.
    pub failed: Vec<String>,
}

/// Route the board's unrouted connections (of one net, if given).
pub fn autoroute(p: &mut Pcb, only: Option<&str>) -> Outcome {
    let mut out = Outcome { routed: 0, tracks: 0, vias: 0, failed: vec![] };
    let mut tried: Vec<(String, [i64; 4])> = vec![];
    for _pass in 0..3 {
        let nl = p.netlist();
        p.retag_copper(&nl);
        let pads = p.pads(&nl);
        let fills = pcb::fill_zones(p, &nl, &pads);
        let its = pcb::items(p, &nl, &pads, &fills);
        let mut air = pcb::ratsnest(&its, &pads, &fills);
        air.retain(|a| only.is_none_or(|n| n == a.net));
        let key = |a: &pcb::Airwire| -> (String, [i64; 4]) { (a.net.clone(), [a.a[0], a.a[1], a.b[0], a.b[1]].map(|v| (v * 1000.0).round() as i64)) };
        air.retain(|a| !tried.contains(&key(a)));
        if air.is_empty() {
            break;
        }
        air.sort_by(|a, b| pcb::dist(a.a, a.b).total_cmp(&pcb::dist(b.a, b.b)));
        let base: Vec<Item> = pcb::items(p, &nl, &pads, &[]);
        let mut grid = build(p, &nl, &base, &[], None);
        let ids: HashMap<String, i32> = nl.nets.iter().enumerate().map(|(i, n)| (n.name.clone(), i as i32)).collect();
        for a in &air {
            tried.push(key(a));
            let net = ids.get(&a.net).copied().unwrap_or(WALL);
            let nr = p.net_rules(&nl, &a.net);
            let vmi = grid.find(nr.via_d / 2.0, nr.clearance, true);
            let mut path = None;
            // the net's width, then the board's narrower signal width when that doesn't fit
            let mut widths = vec![nr.track];
            if p.board.rules.track < nr.track - 1e-9 {
                widths.push(p.board.rules.track);
            }
            for w in widths {
                let mi = grid.find(w / 2.0, nr.clearance, false);
                let la = layers_at(p, &pads, a.a, &a.net);
                let lb = layers_at(p, &pads, a.b, &a.net);
                if let Some(pth) = search(&grid, mi, Some(vmi), net, a.a, &la, a.b, &lb, 4_000_000) {
                    path = Some((pth, w));
                    break;
                }
            }
            let Some((pth, w)) = path else {
                out.failed.push(a.net.clone());
                continue;
            };
            let (tracks, vias) = to_copper(p, &pth, a.a, a.b, w, &nr, &a.net);
            for t in &tracks {
                grid.mark(&Cu::Seg { a: t.a, b: t.b, r: t.width / 2.0 }, Some(t.layer), net, nr.clearance);
            }
            for v in &vias {
                grid.mark(&Cu::Seg { a: v.at, b: v.at, r: v.d / 2.0 }, None, net, nr.clearance);
            }
            out.routed += 1;
            out.tracks += tracks.len();
            out.vias += vias.len();
            p.board.tracks.extend(tracks);
            p.board.vias.extend(vias);
        }
    }
    out.failed.sort();
    out.failed.dedup();
    out
}

/// Layers the net has copper on at a point.
fn layers_at(p: &Pcb, pads: &[WPad], at: P, net: &str) -> Vec<Layer> {
    let mut v = vec![];
    for l in [Layer::Top, Layer::Bottom] {
        let pad = pads.iter().any(|x| x.on(l) && pcb::dist(x.at, at) < 1e-6);
        let track = p.board.tracks.iter().any(|t| t.layer == l && t.net == net && (pcb::dist(t.a, at) < 1e-6 || pcb::dist(t.b, at) < 1e-6));
        let via = p.board.vias.iter().any(|x| pcb::dist(x.at, at) < 1e-6);
        if pad || track || via {
            v.push(l);
        }
    }
    if v.is_empty() {
        v = vec![Layer::Top, Layer::Bottom];
    }
    v
}

impl Grid {
    /// The map for a half-width and clearance (the closest one made).
    fn find(&self, h: f64, c: f64, via: bool) -> usize {
        let mut best = 0;
        let mut d = f64::INFINITY;
        for (i, m) in self.maps.iter().enumerate() {
            let e = (m.h - h).abs() + (m.c - c).abs() + if m.via != via { 1e3 } else { 0.0 };
            if e < d {
                d = e;
                best = i;
            }
        }
        best
    }
}

/// The routing grid: copper, holes, keepouts and the edge marked on a map per profile (and the
/// `extra` ones); `alias` routes one net as another's (a pair's negative as its positive).
fn build(p: &Pcb, nl: &pcb::Netlist, base: &[Item], extra: &[(f64, f64, bool)], alias: Option<(&str, &str)>) -> Grid {
    let r = &p.board.rules;
    let (lo, hi) = p.board.bbox();
    let size = [hi[0] - lo[0], hi[1] - lo[1]];
    // 0.1 mm cells, coarser on big boards
    let g = (((size[0] * size[1]) / 1.2e6).sqrt()).max(0.1);
    let nx = ((size[0] / g).ceil() as usize).max(1);
    let ny = ((size[1] / g).ceil() as usize).max(1);
    let n = nx * ny;
    // a map for every (half-width, clearance) the nets route with, tracks and vias apart
    let mut profiles: Vec<(f64, f64, bool)> = vec![];
    let mut add = |h: f64, c: f64, via: bool| {
        if !profiles.iter().any(|x| (x.0 - h).abs() < 1e-6 && (x.1 - c).abs() < 1e-6 && x.2 == via) {
            profiles.push((h, c, via));
        }
    };
    add(r.track / 2.0, r.clearance, false);
    add(r.via_d / 2.0, r.clearance, true);
    for net in &nl.nets {
        let nr = p.net_rules(nl, &net.name);
        add(nr.track / 2.0, nr.clearance, false);
        add(r.track.min(nr.track) / 2.0, nr.clearance, false);
        add(nr.via_d / 2.0, nr.clearance, true);
    }
    for &(h, c, via) in extra {
        add(h, c, via);
    }
    let mut grid = Grid { lo, g, nx, ny, maps: profiles.iter().map(|&(h, c, via)| Map { h, c, via, m: [vec![FREE; n], vec![FREE; n]] }).collect() };
    // the edge
    for y in 0..ny {
        for x in 0..nx {
            let c = [lo[0] + (x as f64 + 0.5) * g, lo[1] + (y as f64 + 0.5) * g];
            let d = if pcb::inside(&p.board.outline, c) { pcb::outline_dist(&p.board.outline, c) } else { -1.0 };
            for map in &mut grid.maps {
                if d < r.edge + map.h {
                    map.m[0][y * nx + x] = WALL;
                    map.m[1][y * nx + x] = WALL;
                }
            }
        }
    }
    let ids: HashMap<&str, i32> = nl.nets.iter().enumerate().map(|(i, n)| (n.name.as_str(), i as i32)).collect();
    let mut cl: HashMap<&str, f64> = HashMap::new();
    for it in base {
        let name = match alias {
            Some((from, to)) if it.net == from => to,
            _ => it.net.as_str(),
        };
        let net = if it.net.is_empty() { WALL } else { ids.get(name).copied().unwrap_or(WALL) };
        let c = *cl.entry(it.net.as_str()).or_insert_with(|| p.net_rules(nl, &it.net).clearance);
        grid.mark(&it.cu, it.layer, net, c);
    }
    for h in &p.board.holes {
        grid.mark(&Cu::Seg { a: h.at, b: h.at, r: h.d / 2.0 }, None, WALL, r.clearance);
    }
    let vias: Vec<usize> = (0..grid.maps.len()).filter(|&i| grid.maps[i].via).collect();
    let tracks: Vec<usize> = (0..grid.maps.len()).filter(|&i| !grid.maps[i].via).collect();
    // no vias in surface mount pads, even of their own net (solder would wick down the hole)
    for it in base.iter().filter(|it| matches!(it.kind, pcb::ItemKind::Pad(_)) && it.layer.is_some()) {
        grid.mark_maps(&vias, &it.cu, it.layer, WALL, Some(r.clearance));
    }
    // keepouts
    for k in p.board.keepouts.iter().filter(|k| k.outline.len() >= 3) {
        let area = Cu::Poly(k.outline.clone());
        if k.tracks {
            grid.mark_maps(&tracks, &area, k.layer, WALL, None);
        }
        if k.vias {
            grid.mark_maps(&vias, &area, None, WALL, None);
        }
    }
    grid
}

const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];

/// A* from `a` to `b` on map `mi` (track width) for `net`; returns cells (x, y, layer).
fn search(grid: &Grid, mi: usize, vmi: Option<usize>, net: i32, a: P, la: &[Layer], b: P, lb: &[Layer], limit: usize) -> Option<Vec<(usize, usize, usize)>> {
    let (nx, ny) = (grid.nx, grid.ny);
    let (sx, sy) = grid.cell(a)?;
    let (tx, ty) = grid.cell(b)?;
    let n = nx * ny;
    let st = |x: usize, y: usize, l: usize| l * n + y * nx + x;
    let mut cost = vec![f32::INFINITY; 2 * n];
    let mut came = vec![u32::MAX; 2 * n];
    let mut heap = BinaryHeap::new();
    let h = |x: usize, y: usize| -> f32 {
        let (dx, dy) = ((x as i32 - tx as i32).abs() as f32, (y as i32 - ty as i32).abs() as f32);
        dx.max(dy) + (std::f32::consts::SQRT_2 - 1.0) * dx.min(dy)
    };
    let via_cost = (1.2 / grid.g) as f32 + 4.0;
    let track = &grid.maps[mi].m;
    let via = &grid.maps[vmi.unwrap_or(0)].m;
    // the start and end cells sit on the net's own copper
    for l in la {
        let s = st(sx, sy, li(*l));
        cost[s] = 0.0;
        heap.push((Reverse((h(sx, sy) * 64.0) as u64), s as u32));
    }
    let goal: Vec<usize> = lb.iter().map(|l| li(*l)).collect();
    let mut expanded = 0usize;
    while let Some((_, s)) = heap.pop() {
        let s = s as usize;
        let (l, rest) = (s / n, s % n);
        let (x, y) = (rest % nx, rest / nx);
        if x == tx && y == ty && goal.contains(&l) {
            let mut path = vec![];
            let mut c = s;
            loop {
                let (l, rest) = (c / n, c % n);
                path.push((rest % nx, rest / nx, l));
                if came[c] == u32::MAX {
                    break;
                }
                c = came[c] as usize;
            }
            path.reverse();
            return Some(path);
        }
        expanded += 1;
        if expanded > limit {
            return None;
        }
        let g0 = cost[s];
        // the direction we came in, for the turn penalty
        let pd = if came[s] != u32::MAX {
            let p = came[s] as usize % n;
            Some(((x as i32 - (p % nx) as i32), (y as i32 - (p / nx) as i32)))
        } else {
            None
        };
        for (k, (dx, dy)) in DIRS.iter().enumerate() {
            let (x2, y2) = (x as i32 + dx, y as i32 + dy);
            if x2 < 0 || y2 < 0 || x2 >= nx as i32 || y2 >= ny as i32 {
                continue;
            }
            let (x2, y2) = (x2 as usize, y2 as usize);
            let t = y2 * nx + x2;
            let is_goal = x2 == tx && y2 == ty;
            if !free(track[l][t], net) && !is_goal {
                continue;
            }
            // diagonals may not cut a blocked corner
            if k % 2 == 1 && (!free(track[l][y * nx + x2], net) || !free(track[l][y2 * nx + x], net)) {
                continue;
            }
            let mut c = if k % 2 == 1 { std::f32::consts::SQRT_2 } else { 1.0 };
            // top prefers x, bottom prefers y
            let along = if l == 0 { *dy == 0 } else { *dx == 0 };
            if !along {
                c *= if k % 2 == 1 { 1.1 } else { 1.25 };
            }
            if pd.is_some_and(|d| d != (*dx, *dy)) {
                c += 1.5;
            }
            let s2 = st(x2, y2, l);
            let g2 = g0 + c;
            if g2 < cost[s2] {
                cost[s2] = g2;
                came[s2] = s as u32;
                heap.push((Reverse(((g2 + h(x2, y2)) * 64.0) as u64), s2 as u32));
            }
        }
        // a via to the other layer
        let here = y * nx + x;
        let start_or_goal = (x == sx && y == sy) || (x == tx && y == ty);
        if vmi.is_some() && !start_or_goal && free(via[0][here], net) && free(via[1][here], net) {
            let s2 = st(x, y, 1 - l);
            let g2 = g0 + via_cost;
            if g2 < cost[s2] {
                cost[s2] = g2;
                came[s2] = s as u32;
                heap.push((Reverse(((g2 + h(x, y)) * 64.0) as u64), s2 as u32));
            }
        }
    }
    None
}

/// Turn a cell path into tracks (straight runs merged) and vias.
fn to_copper(p: &Pcb, path: &[(usize, usize, usize)], a: P, b: P, w: f64, nr: &pcb::NetRules, net: &str) -> (Vec<Track>, Vec<Via>) {
    let (lo, g) = {
        let (lo, hi) = p.board.bbox();
        let size = [hi[0] - lo[0], hi[1] - lo[1]];
        (lo, (((size[0] * size[1]) / 1.2e6).sqrt()).max(0.1))
    };
    let centre = |x: usize, y: usize| [lo[0] + (x as f64 + 0.5) * g, lo[1] + (y as f64 + 0.5) * g];
    let layer = |l: usize| if l == 0 { Layer::Top } else { Layer::Bottom };
    let mut tracks = vec![];
    let mut vias = vec![];
    let mut run: Vec<P> = vec![a];
    let mut cur = path[0].2;
    for (i, &(x, y, l)) in path.iter().enumerate() {
        let c = if i == path.len() - 1 { b } else if i == 0 { a } else { centre(x, y) };
        if l != cur {
            let at = *run.last().unwrap();
            push_run(&mut tracks, &run, w, layer(cur), net);
            vias.push(Via { at, d: nr.via_d, drill: nr.via_drill, net: net.into() });
            run = vec![at];
            cur = l;
        }
        if pcb::dist(*run.last().unwrap(), c) > 1e-9 {
            run.push(c);
        }
    }
    push_run(&mut tracks, &run, w, layer(cur), net);
    (tracks, vias)
}

fn push_run(out: &mut Vec<Track>, run: &[P], w: f64, layer: Layer, net: &str) {
    if run.len() < 2 {
        return;
    }
    // keep the corners only
    let mut pts = vec![run[0]];
    for i in 1..run.len() - 1 {
        let (p, q, r) = (*pts.last().unwrap(), run[i], run[i + 1]);
        let (d1, d2) = (pcb::sub(q, p), pcb::sub(r, q));
        let cr = d1[0] * d2[1] - d1[1] * d2[0];
        let l = (d1[0].hypot(d1[1]) * d2[0].hypot(d2[1])).max(1e-12);
        if (cr / l).abs() > 1e-6 || d1[0] * d2[0] + d1[1] * d2[1] < 0.0 {
            pts.push(q);
        }
    }
    pts.push(*run.last().unwrap());
    for s in pts.windows(2) {
        if pcb::dist(s[0], s[1]) > 1e-9 {
            out.push(Track { a: s[0], b: s[1], width: w, layer, net: net.into() });
        }
    }
}

/// The corners of a polyline (collinear points dropped).
fn corners(run: &[P]) -> Vec<P> {
    if run.len() < 3 {
        return run.to_vec();
    }
    let mut pts = vec![run[0]];
    for i in 1..run.len() - 1 {
        let (p, q, r) = (*pts.last().unwrap(), run[i], run[i + 1]);
        let (d1, d2) = (pcb::sub(q, p), pcb::sub(r, q));
        let cr = d1[0] * d2[1] - d1[1] * d2[0];
        let l = (d1[0].hypot(d1[1]) * d2[0].hypot(d2[1])).max(1e-12);
        if (cr / l).abs() > 1e-6 || d1[0] * d2[0] + d1[1] * d2[1] < 0.0 {
            pts.push(q);
        }
    }
    pts.push(*run.last().unwrap());
    pts.dedup_by(|a, b| pcb::dist(*a, *b) < 1e-9);
    pts
}

/// A polyline moved sideways by `d` (left of its direction), with mitred corners.
pub fn offset_line(pts: &[P], d: f64) -> Vec<P> {
    let n = pts.len();
    if n < 2 {
        return pts.to_vec();
    }
    let left = |a: P, b: P| {
        let l = pcb::dist(a, b).max(1e-12);
        [-(b[1] - a[1]) / l, (b[0] - a[0]) / l]
    };
    (0..n)
        .map(|i| {
            let n1 = if i > 0 { left(pts[i - 1], pts[i]) } else { left(pts[0], pts[1]) };
            let n2 = if i + 1 < n { left(pts[i], pts[i + 1]) } else { n1 };
            let m = [n1[0] + n2[0], n1[1] + n2[1]];
            let ml = m[0].hypot(m[1]).max(1e-12);
            let m = [m[0] / ml, m[1] / ml];
            let k = d / (m[0] * n1[0] + m[1] * n1[1]).max(0.3);
            [pts[i][0] + m[0] * k, pts[i][1] + m[1] * k]
        })
        .collect()
}

/// The cell path's centres as a polyline from `a` to `b`, corners only.
fn path_line(grid: &Grid, path: &[(usize, usize, usize)], a: P, b: P) -> Vec<P> {
    let mut run = vec![a];
    for (i, &(x, y, _)) in path.iter().enumerate() {
        if i == 0 || i == path.len() - 1 {
            continue;
        }
        run.push(grid.centre(x, y));
    }
    run.push(b);
    corners(&run)
}

pub struct PairOutcome {
    pub tracks: usize,
    pub layer: Layer,
    pub p_len: f64,
    pub n_len: f64,
}

/// Route a differential pair together: the two tracks run side by side at the pair's gap, from
/// the pads where the nets start (the closest positive and negative pads) to where they end
/// (the farthest), on one layer. Other connections of the nets are left to the autorouter.
pub fn route_pair(p: &mut Pcb, d: &pcb::DiffPair) -> Result<PairOutcome, String> {
    let nl = p.netlist();
    let (Some(pi), Some(ni)) = (nl.by_name(&d.p), nl.by_name(&d.n)) else {
        return Err(format!("pair {}: nets {} and {} must both be on the schematic", d.name, d.p, d.n));
    };
    let pads = p.pads(&nl);
    let pp: Vec<&WPad> = pads.iter().filter(|x| x.net == Some(pi)).collect();
    let np: Vec<&WPad> = pads.iter().filter(|x| x.net == Some(ni)).collect();
    if pp.len() < 2 || np.len() < 2 {
        return Err(format!("pair {}: each net needs two pads to join", d.name));
    }
    let rp = p.net_rules(&nl, &d.p);
    let rn = p.net_rules(&nl, &d.n);
    let w = if d.width > 0.0 { d.width } else { rp.track };
    let g = if d.gap > 0.0 { d.gap } else { rp.clearance.max(rn.clearance) };
    let near = |q: P, set: &[&WPad]| -> usize { (0..set.len()).min_by(|&a, &b| pcb::dist(set[a].at, q).total_cmp(&pcb::dist(set[b].at, q))).unwrap() };
    // start where the two nets come closest, end at the positive pad farthest from there
    let mut s = (0, 0, f64::INFINITY);
    for (i, a) in pp.iter().enumerate() {
        let j = near(a.at, &np);
        let l = pcb::dist(a.at, np[j].at);
        if l < s.2 {
            s = (i, j, l);
        }
    }
    let (sp, sn) = (pp[s.0], np[s.1]);
    let ep = pp.iter().enumerate().filter(|(i, _)| *i != s.0).max_by(|a, b| pcb::dist(a.1.at, sp.at).total_cmp(&pcb::dist(b.1.at, sp.at))).map(|x| *x.1).unwrap();
    let en = np[near(ep.at, &np)];
    if std::ptr::eq(en, sn) {
        return Err(format!("pair {}: no second negative pad near {}", d.name, d.p));
    }
    let mid = |a: P, b: P| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let (sa, sb) = (mid(sp.at, sn.at), mid(ep.at, en.at));
    let h = g / 2.0 + w;
    let c = rp.clearance.max(rn.clearance);
    let base: Vec<Item> = pcb::items(p, &nl, &pads, &[]);
    let grid = build(p, &nl, &base, &[(h, c, false)], Some((&d.n, &d.p)));
    let mi = grid.find(h, c, false);
    let common = |a: &WPad, b: &WPad| -> Vec<Layer> { [Layer::Top, Layer::Bottom].into_iter().filter(|l| a.on(*l) && b.on(*l)).collect() };
    let mut layers = common(sp, sn);
    layers.retain(|l| common(ep, en).contains(l));
    if layers.is_empty() {
        return Err(format!("pair {}: its pads are on different sides; route it by hand with a via", d.name));
    }
    // leave each pad pair square to the line between its pads, so the two tracks fan in
    // straight and don't cross
    let escape = |a: &WPad, b: &WPad, toward: P| -> P {
        let m = mid(a.at, b.at);
        let ax = pcb::sub(b.at, a.at);
        let l = (ax[0].hypot(ax[1])).max(1e-9);
        let mut perp = [-ax[1] / l, ax[0] / l];
        let t = pcb::sub(toward, m);
        if perp[0] * t[0] + perp[1] * t[1] < 0.0 {
            perp = [-perp[0], -perp[1]];
        }
        let e = a.size[0].max(a.size[1]).max(b.size[0].max(b.size[1])) / 2.0 + g + w;
        [m[0] + perp[0] * e, m[1] + perp[1] * e]
    };
    let (sa2, sb2) = (escape(sp, sn, sb), escape(ep, en, sa));
    let mut found = None;
    for l in layers {
        if let Some(path) = search(&grid, mi, None, pi as i32, sa2, &[l], sb2, &[l], 4_000_000) {
            found = Some((path, l));
            break;
        }
    }
    let Some((path, layer)) = found else {
        return Err(format!("pair {}: no way through for two {w} mm tracks {g} mm apart; move parts apart or narrow the pair", d.name));
    };
    let mut centre = vec![sa];
    centre.extend(path_line(&grid, &path, sa2, sb2));
    centre.push(sb);
    let centre = corners(&centre);
    let off = (g + w) / 2.0;
    let dir = pcb::sub(centre[1], centre[0]);
    let to_p = pcb::sub(sp.at, sa);
    let p_left = dir[0] * to_p[1] - dir[1] * to_p[0] > 0.0;
    let side = if p_left { 1.0 } else { -1.0 };
    let line_p = offset_line(&centre, side * off);
    let line_n = offset_line(&centre, -side * off);
    let mut tracks = vec![];
    for (line, a, b, net) in [(line_p, sp.at, ep.at, &d.p), (line_n, sn.at, en.at, &d.n)] {
        let mut full = vec![a];
        full.extend(line);
        full.push(b);
        let full = corners(&full);
        for s in full.windows(2) {
            if pcb::dist(s[0], s[1]) > 1e-9 {
                tracks.push(Track { a: s[0], b: s[1], width: w, layer, net: net.clone() });
            }
        }
    }
    let n = tracks.len();
    p.board.tracks.extend(tracks);
    Ok(PairOutcome { tracks: n, layer, p_len: crate::pcb_tune::net_length(p, &d.p), n_len: crate::pcb_tune::net_length(p, &d.n) })
}

/// Walkaround for interactive routing: a grid for one net and width, searched from the last
/// corner to the cursor on one layer, so the track preview flows round other copper.
pub struct Walk {
    grid: Grid,
    net: i32,
    mi: usize,
}

impl Walk {
    pub fn new(p: &Pcb, net: &str, width: f64) -> Walk {
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let base: Vec<Item> = pcb::items(p, &nl, &pads, &[]);
        let c = p.net_rules(&nl, net).clearance;
        let grid = build(p, &nl, &base, &[(width / 2.0, c, false)], None);
        let mi = grid.find(width / 2.0, c, false);
        let id = nl.by_name(net).map(|i| i as i32).unwrap_or(WALL);
        Walk { grid, net: id, mi }
    }
    /// A clear path from `a` to `b` on `layer`, corners only; None when there is none nearby.
    pub fn path(&self, a: P, b: P, layer: Layer) -> Option<Vec<P>> {
        let path = search(&self.grid, self.mi, None, self.net, a, &[layer], b, &[layer], 150_000)?;
        Some(path_line(&self.grid, &path, a, b))
    }
}

/// Put parts on the board: the biggest first in the middle, then each part next to the parts
/// it shares the most nets with, at the free spot nearest them. `all` lays out every part again;
/// otherwise only parts not on the board move. Returns how many were placed.
pub fn auto_place(p: &mut Pcb, all: bool) -> usize {
    let (lo, hi) = p.board.bbox();
    let on_board = |p: &Pcb, i: usize| {
        let (a, b) = p.court_world(&p.parts[i]);
        pcb::inside(&p.board.outline, a) && pcb::inside(&p.board.outline, b)
    };
    let mut todo: Vec<usize> = (0..p.parts.len()).filter(|&i| all || !on_board(p, i)).collect();
    let area = |p: &Pcb, i: usize| {
        let (a, b) = p.parts[i].footprint().court;
        (b[0] - a[0]) * (b[1] - a[1])
    };
    // how many nets two parts share
    let nl = p.netlist();
    let n = p.parts.len();
    let mut links = vec![vec![0usize; n]; n];
    for net in &nl.nets {
        let mut ps: Vec<usize> = net.pins.iter().map(|x| x.0).collect();
        ps.sort();
        ps.dedup();
        // big nets (ground, supply) say little about where a part belongs
        let w = if ps.len() > 6 { 0 } else { 1 };
        for a in &ps {
            for b in &ps {
                if a != b {
                    links[*a][*b] += w;
                }
            }
        }
    }
    let mut placed: Vec<usize> = (0..n).filter(|i| !todo.contains(i)).collect();
    let mut taken: Vec<(P, P)> = placed.iter().map(|&i| p.court_world(&p.parts[i])).collect();
    taken.extend(p.board.holes.iter().map(|h| ([h.at[0] - h.d / 2.0 - 0.5, h.at[1] - h.d / 2.0 - 0.5], [h.at[0] + h.d / 2.0 + 0.5, h.at[1] + h.d / 2.0 + 0.5])));
    let margin = p.board.rules.edge + 0.7;
    let gap = 1.5;
    let middle = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let mut count = 0;
    while !todo.is_empty() {
        // the part most tied to what is placed, else the biggest
        let k = (0..todo.len())
            .max_by(|&a, &b| {
                let la: usize = placed.iter().map(|&q| links[todo[a]][q]).sum();
                let lb: usize = placed.iter().map(|&q| links[todo[b]][q]).sum();
                la.cmp(&lb).then(area(p, todo[a]).total_cmp(&area(p, todo[b])))
            })
            .unwrap();
        let i = todo.remove(k);
        let mut tw = 0.0;
        let mut target = [0.0, 0.0];
        for &q in &placed {
            let w = links[i][q] as f64;
            if w > 0.0 {
                target = pcb::add(target, [p.parts[q].at[0] * w, p.parts[q].at[1] * w]);
                tw += w;
            }
        }
        let target = if tw > 0.0 { [target[0] / tw, target[1] / tw] } else { middle };
        let keep_rot = !all && on_board(p, i);
        if !keep_rot {
            p.parts[i].rot = 0.0;
        }
        let base_rot = p.parts[i].rot;
        let mut best: Option<(f64, P, f64)> = None;
        for rot in [base_rot, base_rot + 90.0] {
            let mut q = p.parts[i].clone();
            q.rot = rot;
            q.at = [0.0, 0.0];
            let (a, b) = p.court_world(&q);
            let (w, h) = (b[0] - a[0], b[1] - a[1]);
            let mut y = lo[1] + margin;
            while y + h <= hi[1] - margin + 1e-9 {
                let mut x = lo[0] + margin;
                while x + w <= hi[0] - margin + 1e-9 {
                    let (bl, tr) = ([x, y], [x + w, y + h]);
                    let at = [x - a[0], y - a[1]];
                    // turning costs a little, so parts stay square to each other when it doesn't help
                    let score = pcb::dist(at, target) + if rot != base_rot { 0.5 } else { 0.0 };
                    if best.is_none_or(|b| score < b.0) {
                        let corners = [bl, tr, [bl[0], tr[1]], [tr[0], bl[1]]];
                        let ok = corners.iter().all(|c| pcb::inside(&p.board.outline, *c) && pcb::outline_dist(&p.board.outline, *c) >= margin - 1e-9)
                            && taken.iter().all(|(l, u)| bl[0] >= u[0] + gap || tr[0] <= l[0] - gap || bl[1] >= u[1] + gap || tr[1] <= l[1] - gap);
                        if ok {
                            best = Some((score, at, rot));
                        }
                    }
                    x += 0.5;
                }
                y += 0.5;
            }
        }
        if let Some((_, s, rot)) = best {
            p.parts[i].at = s;
            p.parts[i].rot = rot.rem_euclid(360.0);
            taken.push(p.court_world(&p.parts[i]));
            count += 1;
        }
        placed.push(i);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_the_led_board() {
        let mut p = crate::pcb::tests::led_board();
        let out = autoroute(&mut p, None);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.routed, 3);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let issues = pcb::drc(&p, &nl, &pads, &[]);
        assert!(issues.iter().all(|i| !i.error), "{issues:?}");
    }

    #[test]
    fn no_vias_in_surface_mount_pads() {
        // the LED under the resistor on the bottom: its net needs a via, and the bottom's
        // preferred y direction pulls it towards the resistor's pad
        let mut p = crate::pcb::tests::led_board();
        p.parts[0].at = [25.0, 8.0];
        p.parts[0].rot = 90.0;
        p.parts[1].at = [25.0, 32.0];
        p.parts[1].rot = 90.0;
        p.parts[1].bottom = true;
        let out = autoroute(&mut p, None);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert!(out.vias > 0);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        for v in &p.board.vias {
            let via = Cu::Seg { a: v.at, b: v.at, r: v.d / 2.0 };
            for pad in pads.iter().filter(|x| x.layer.is_some()) {
                assert!(via.gap(&pad.cu()) > 0.0, "via at {:?} in {} pad {}", v.at, p.parts[pad.part].reference, pad.num);
            }
        }
        let issues = pcb::drc(&p, &nl, &pads, &[]);
        assert!(issues.iter().all(|i| !i.error), "{issues:?}");
    }

    #[test]
    fn places_parts_on_the_board() {
        let mut p = crate::pcb::tests::led_board();
        for part in &mut p.parts {
            part.at = [100.0, 100.0];
        }
        assert_eq!(auto_place(&mut p, false), 3);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let issues = pcb::drc(&p, &nl, &pads, &[]);
        assert!(!issues.iter().any(|i| i.message.contains("overlap") || i.message.contains("not inside")), "{issues:?}");
    }

    #[test]
    fn routes_a_differential_pair_side_by_side() {
        use crate::pcb::{Label, LabelKind};
        let mut p = Pcb::new("pair");
        p.board.outline = pcb::rect_outline(60.0, 30.0, 0.0);
        let a = p.add_part("J", Some("PinHeader_1x02"), None, [0.0, 0.0]).unwrap();
        let b = p.add_part("J", Some("PinHeader_1x02"), None, [30.0, 0.0]).unwrap();
        let pins = p.sch_pins();
        for (part, num, net) in [(a, "1", "D_P"), (a, "2", "D_N"), (b, "1", "D_P"), (b, "2", "D_N")] {
            let at = pins.iter().find(|x| x.part == part && x.num == num).unwrap().at;
            p.labels.push(Label { at, text: net.into(), kind: LabelKind::Net, rot: 0 });
        }
        p.parts[a].at = [8.0, 15.0];
        p.parts[b].at = [50.0, 12.0];
        // a part in the way
        let r = p.add_part("R", Some("R_1206"), None, [0.0, 20.0]).unwrap();
        p.parts[r].at = [30.0, 14.0];
        let d = pcb::DiffPair { name: "D".into(), p: "D_P".into(), n: "D_N".into(), width: 0.3, gap: 0.25 };
        let o = route_pair(&mut p, &d).unwrap();
        assert!(o.tracks >= 4);
        assert!((o.p_len - o.n_len).abs() < 3.0, "{} {}", o.p_len, o.n_len);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let issues = pcb::drc(&p, &nl, &pads, &[]);
        assert!(!issues.iter().any(|i| i.rule == "short" || i.rule == "unrouted" || i.rule == "edge"), "{issues:#?}");
        // the two tracks run at the gap along the middle of the route
        let tp: Vec<&Track> = p.board.tracks.iter().filter(|t| t.net == "D_P").collect();
        let tn: Vec<&Track> = p.board.tracks.iter().filter(|t| t.net == "D_N").collect();
        let mid = tp.iter().max_by(|x, y| pcb::dist(x.a, x.b).total_cmp(&pcb::dist(y.a, y.b))).unwrap();
        let m = [(mid.a[0] + mid.b[0]) / 2.0, (mid.a[1] + mid.b[1]) / 2.0];
        let gap = tn.iter().map(|t| pcb::seg_point(t.a, t.b, m)).fold(f64::INFINITY, f64::min) - 0.3;
        assert!((gap - 0.25).abs() < 0.02, "gap {gap}");
        // walkaround finds a way round the resistor on the top layer
        let w = Walk::new(&p, "GND", 0.25);
        let path = w.path([20.0, 5.0], [40.0, 5.0], Layer::Top).unwrap();
        assert!(path.len() >= 2);
    }
}
