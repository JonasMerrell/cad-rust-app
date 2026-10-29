//! A grid autorouter for PCB tabs: A* on two layers with vias, one airwire at a time, shortest
//! first. Copper of other nets is kept out by its clearance; zones are refilled afterwards and
//! anything they no longer connect is routed in another pass.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::pcb::{self, Cu, Item, Layer, Pcb, Track, Via, WPad};
use crate::pcb_lib::P;

const FREE: i32 = -1;
const WALL: i32 = -2;

/// Per layer, which cells something of half-width `h` may use: FREE, WALL, or only one net's.
struct Map {
    h: f64,
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

    /// Mark the cells within `extra` (plus each map's half-width and the clearance) of a piece of copper.
    fn mark(&mut self, cu: &Cu, layer: Option<Layer>, net: i32, clearance: f64) {
        let (lo, hi) = cu.bbox();
        for mi in 0..self.maps.len() {
            let r = clearance + self.maps[mi].h + self.g * 0.5;
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
        let mut grid = build(p, &base);
        let ids: HashMap<String, i32> = nl.nets.iter().enumerate().map(|(i, n)| (n.name.clone(), i as i32)).collect();
        for a in &air {
            tried.push(key(a));
            let net = ids.get(&a.net).copied().unwrap_or(WALL);
            let w = p.track_width(&nl, &a.net);
            let mut path = None;
            for mi in [if w > p.board.rules.track + 1e-9 { 1 } else { 0 }, 0] {
                let la = layers_at(p, &pads, a.a, &a.net);
                let lb = layers_at(p, &pads, a.b, &a.net);
                if let Some(pth) = search(&grid, mi, net, a.a, &la, a.b, &lb) {
                    path = Some((pth, if mi == 1 { p.board.rules.power_track } else { p.board.rules.track }));
                    break;
                }
            }
            let Some((pth, w)) = path else {
                out.failed.push(a.net.clone());
                continue;
            };
            let (tracks, vias) = to_copper(p, &pth, a.a, a.b, w, &a.net);
            for t in &tracks {
                grid.mark(&Cu::Seg { a: t.a, b: t.b, r: t.width / 2.0 }, Some(t.layer), net, p.board.rules.clearance);
            }
            for v in &vias {
                grid.mark(&Cu::Seg { a: v.at, b: v.at, r: v.d / 2.0 }, None, net, p.board.rules.clearance);
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

fn build(p: &Pcb, base: &[Item]) -> Grid {
    let r = &p.board.rules;
    let (lo, hi) = p.board.bbox();
    let size = [hi[0] - lo[0], hi[1] - lo[1]];
    // 0.1 mm cells, coarser on big boards
    let g = (((size[0] * size[1]) / 1.2e6).sqrt()).max(0.1);
    let nx = ((size[0] / g).ceil() as usize).max(1);
    let ny = ((size[1] / g).ceil() as usize).max(1);
    let n = nx * ny;
    let hs = [r.track / 2.0, r.power_track / 2.0, r.via_d / 2.0];
    let mut grid = Grid { lo, g, nx, ny, maps: hs.iter().map(|&h| Map { h, m: [vec![FREE; n], vec![FREE; n]] }).collect() };
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
    let nl = p.netlist();
    let ids: HashMap<&str, i32> = nl.nets.iter().enumerate().map(|(i, n)| (n.name.as_str(), i as i32)).collect();
    for it in base {
        let net = if it.net.is_empty() { WALL } else { ids.get(it.net.as_str()).copied().unwrap_or(WALL) };
        grid.mark(&it.cu, it.layer, net, r.clearance);
    }
    for h in &p.board.holes {
        grid.mark(&Cu::Seg { a: h.at, b: h.at, r: h.d / 2.0 }, None, WALL, r.clearance);
    }
    grid
}

const DIRS: [(i32, i32); 8] = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)];

/// A* from `a` to `b` on map `mi` (track width) for `net`; returns cells (x, y, layer).
fn search(grid: &Grid, mi: usize, net: i32, a: P, la: &[Layer], b: P, lb: &[Layer]) -> Option<Vec<(usize, usize, usize)>> {
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
    let via = &grid.maps[2].m;
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
        if expanded > 4_000_000 {
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
        if !start_or_goal && free(via[0][here], net) && free(via[1][here], net) {
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
fn to_copper(p: &Pcb, path: &[(usize, usize, usize)], a: P, b: P, w: f64, net: &str) -> (Vec<Track>, Vec<Via>) {
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
            vias.push(Via { at, d: p.board.rules.via_d, drill: p.board.rules.via_drill, net: net.into() });
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
}
