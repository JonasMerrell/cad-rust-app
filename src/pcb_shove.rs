//! Push and shove for interactive routing: the track being routed pushes other nets' tracks and
//! vias out of its way, and those push the ones behind them, instead of only flowing round them.
//!
//! Ported to Rust from KiCad's router (pcbnew/router: SHOVE in pns_shove.cpp, LINE::Walkaround in
//! pns_line.cpp, SegmentHull and OctagonalHull in pns_utils.cpp):
//!
//!   KiRouter - a push-and-(sometimes-)shove PCB router
//!   Copyright (C) 2013-2014 CERN
//!   Copyright The KiCad Developers, see AUTHORS.txt for contributors.
//!   Author: Tomasz Wlostowski <tomasz.wlostowski@cern.ch>
//!
//!   This program is free software: you can redistribute it and/or modify it under the terms of
//!   the GNU General Public License as published by the Free Software Foundation, either version
//!   3 of the License, or (at your option) any later version. This program is distributed in the
//!   hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of
//!   MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
//!   more details. <http://www.gnu.org/licenses/>
//!
//! The port is combined with rCAD under the GNU AGPL-3.0 as section 13 of both licences allows.
//!
//! How it works, as in KiCad: every item has a rank; the routed track (the head) has the highest.
//! A stack holds the items still to check. The top item's nearest collision with a lower ranked
//! item is resolved: a track is re-walked round the hulls (clearance octagons) of the pusher's
//! segments, a via is moved out by the shortest way and drags its tracks with it. Each moved item
//! goes on the stack with a lower rank, so it can push only what is behind it. Pads, holes,
//! keepouts and the board edge can't be pushed: running into one fails the shove, and the router
//! falls back to walking round.

use std::collections::HashMap;

use crate::pcb::{self, Cu, Layer, Pcb, Track, Via, WPad};
use crate::pcb_lib::P;

const HEAD_RANK: i32 = 1 << 20;
/// Extra room on every hull so pushed copper isn't exactly at the clearance (rounding).
const EPS: f64 = 2e-4;
const MAX_ITER: usize = 250;

/// What a shove did: the board's tracks and vias with the pushed ones moved (the head's own
/// track isn't among them).
#[derive(Clone, Debug)]
pub struct Shoved {
    pub tracks: Vec<Track>,
    pub vias: Vec<Via>,
    /// How many tracks and vias moved.
    pub moved: usize,
}

/// The track being routed.
pub struct Head<'a> {
    pub pts: &'a [P],
    pub width: f64,
    pub layer: Layer,
    pub net: &'a str,
}

/// Push other nets' copper out of the way of `head`. None when it can't be done (a pad, hole,
/// keepout or the edge is in the way, or the pushes go round in circles).
pub fn shove(p: &Pcb, pads: &[WPad], pad_nets: &[String], head: &Head) -> Option<Shoved> {
    let mut w = World::new(p, pads, pad_nets, head);
    let mut stack: Vec<(Cur, i32)> = vec![(Cur::Head, HEAD_RANK)];
    let mut iter = 0;
    while let Some((cur, rank)) = stack.last().cloned() {
        iter += 1;
        if iter > MAX_ITER {
            return None;
        }
        let Some(g) = w.geom(&cur) else {
            stack.pop();
            continue;
        };
        match w.nearest(&g, rank) {
            Err(()) => return None,
            Ok(None) => {
                stack.pop();
            }
            Ok(Some(Obs::Track(j))) => {
                let (ids, pts) = w.assemble(j);
                if ids.iter().any(|&i| w.trank[i] >= rank) {
                    return None;
                }
                let t = w.tracks[j].clone();
                let cl = w.cl(&g.net, &t.net);
                let new = shove_line(&w, &g, &pts, t.width, t.layer, &t.net, cl, &ids)?;
                let nid = w.replace(&ids, &new, t.width, t.layer, &t.net, rank - 1);
                stack.push((Cur::Line(nid), rank - 1));
            }
            Ok(Some(Obs::Via(v))) => {
                if w.vrank[v] >= rank {
                    return None;
                }
                let lines = w.push_via(v, &g, rank - 1)?;
                stack.push((Cur::Via(v), rank - 1));
                for ids in lines {
                    stack.push((Cur::Line(ids), rank - 1));
                }
            }
        }
    }
    let moved = w.alive.iter().zip(&w.trank).filter(|(a, r)| **a && **r > 0).count() + w.vrank.iter().filter(|r| **r > 0).count();
    let tracks = w.tracks.iter().zip(&w.alive).filter(|(_, a)| **a).map(|(t, _)| t.clone()).collect();
    Some(Shoved { tracks, vias: w.vias, moved })
}

#[derive(Clone, Debug)]
enum Cur {
    Head,
    /// A pushed track's segments, in order.
    Line(Vec<usize>),
    Via(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Obs {
    Track(usize),
    Via(usize),
}

/// An item's copper: a chain of segments of one width on one layer, or a via.
struct Geom {
    segs: Vec<(P, P)>,
    /// Half the track width, or the via's radius.
    r: f64,
    layer: Option<Layer>,
    via: Option<usize>,
    net: String,
    /// Where the item was pushed from: the head's first point, a via's centre.
    origin: P,
}

struct World<'a> {
    p: &'a Pcb,
    pads: &'a [WPad],
    pad_nets: &'a [String],
    head: Vec<P>,
    head_w: f64,
    head_layer: Layer,
    head_net: String,
    tracks: Vec<Track>,
    alive: Vec<bool>,
    trank: Vec<i32>,
    vias: Vec<Via>,
    vrank: Vec<i32>,
    clear: HashMap<String, f64>,
    layers: Vec<Layer>,
}

/// Copper of two nets that may touch.
fn same(a: &str, b: &str) -> bool {
    !a.is_empty() && a == b
}

impl<'a> World<'a> {
    fn new(p: &'a Pcb, pads: &'a [WPad], pad_nets: &'a [String], head: &Head) -> World<'a> {
        let nl = p.netlist();
        let mut clear = HashMap::new();
        let nets = p.board.tracks.iter().map(|t| t.net.as_str()).chain(p.board.vias.iter().map(|v| v.net.as_str())).chain(pad_nets.iter().map(|s| s.as_str())).chain([head.net]);
        for n in nets {
            if !clear.contains_key(n) {
                clear.insert(n.to_string(), p.net_rules(&nl, n).clearance);
            }
        }
        World {
            p,
            pads,
            pad_nets,
            head: head.pts.to_vec(),
            head_w: head.width,
            head_layer: head.layer,
            head_net: head.net.to_string(),
            tracks: p.board.tracks.clone(),
            alive: vec![true; p.board.tracks.len()],
            trank: vec![0; p.board.tracks.len()],
            vias: p.board.vias.clone(),
            vrank: vec![0; p.board.vias.len()],
            clear,
            layers: p.board.copper_layers(),
        }
    }

    /// The clearance between two nets' copper (the larger of their rules).
    fn cl(&self, a: &str, b: &str) -> f64 {
        let f = |n: &str| self.clear.get(n).copied().unwrap_or(self.p.board.rules.clearance);
        f(a).max(f(b))
    }

    fn geom(&self, c: &Cur) -> Option<Geom> {
        match c {
            Cur::Head => Some(Geom { segs: self.head.windows(2).map(|s| (s[0], s[1])).collect(), r: self.head_w / 2.0, layer: Some(self.head_layer), via: None, net: self.head_net.clone(), origin: self.head[0] }),
            Cur::Line(ids) => {
                if ids.is_empty() || ids.iter().any(|&i| !self.alive[i]) {
                    return None;
                }
                let t = &self.tracks[ids[0]];
                Some(Geom { segs: ids.iter().map(|&i| (self.tracks[i].a, self.tracks[i].b)).collect(), r: t.width / 2.0, layer: Some(t.layer), via: None, net: t.net.clone(), origin: t.a })
            }
            Cur::Via(v) => {
                let v0 = &self.vias[*v];
                Some(Geom { segs: vec![(v0.at, v0.at)], r: v0.d / 2.0, layer: None, via: Some(*v), net: v0.net.clone(), origin: v0.at })
            }
        }
    }

    fn on(&self, g: &Geom, l: Layer) -> bool {
        match (g.layer, g.via) {
            (Some(x), _) => x == l,
            (None, Some(v)) => self.vias[v].on(l),
            _ => false,
        }
    }

    /// The first thing `g` runs into, along its segments: Err for something that can't be
    /// pushed (or ranks as high as `g`), else the track or via to push. Vias come before
    /// tracks, as in KiCad (a via's tracks can't be walked round the pusher while the via sits
    /// in its way).
    fn nearest(&self, g: &Geom, rank: i32) -> Result<Option<Obs>, ()> {
        let mut best: Option<(f64, Obs)> = None;
        let mut consider = |key: f64, o: Obs, orank: i32| -> Result<(), ()> {
            if orank >= rank {
                return Err(());
            }
            // vias first: they sort ahead of every track
            let key = if matches!(o, Obs::Via(_)) { key - 1e9 } else { key };
            if best.is_none_or(|b| key < b.0) {
                best = Some((key, o));
            }
            Ok(())
        };
        let pushed = rank < HEAD_RANK;
        for (k, &(a, b)) in g.segs.iter().enumerate() {
            let along = |q: P| k as f64 + param(a, b, q);
            for (i, t) in self.tracks.iter().enumerate() {
                if !self.alive[i] || same(&t.net, &g.net) || !self.on(g, t.layer) {
                    continue;
                }
                let need = g.r + t.width / 2.0 + self.cl(&g.net, &t.net);
                if pcb::seg_seg(a, b, t.a, t.b) < need - 1e-6 {
                    let q = closer(a, b, t.a, t.b);
                    consider(along(q), Obs::Track(i), self.trank[i])?;
                }
            }
            for (i, v) in self.vias.iter().enumerate() {
                if g.via == Some(i) || same(&v.net, &g.net) || !self.layers.iter().any(|l| v.on(*l) && self.on(g, *l)) {
                    continue;
                }
                let need = g.r + v.d / 2.0 + self.cl(&g.net, &v.net);
                if pcb::seg_point(a, b, v.at) < need - 1e-6 {
                    consider(along(v.at), Obs::Via(i), self.vrank[i])?;
                }
            }
            // the routed track can't cross a pushed one of a higher rank (or the head)
            if g.via.is_some() || pushed {
                if self.on(g, self.head_layer) && !same(&self.head_net, &g.net) {
                    let need = g.r + self.head_w / 2.0 + self.cl(&g.net, &self.head_net);
                    if self.head.windows(2).any(|s| pcb::seg_seg(a, b, s[0], s[1]) < need - 1e-6) {
                        return Err(());
                    }
                }
                if self.fixed_hit(a, b, g) {
                    return Err(());
                }
            } else if self.pad_hit(a, b, g) {
                return Err(());
            }
        }
        Ok(best.map(|b| b.1))
    }

    /// A pad of another net in the way.
    fn pad_hit(&self, a: P, b: P, g: &Geom) -> bool {
        let me = Cu::Seg { a, b, r: g.r };
        self.pads.iter().zip(self.pad_nets).any(|(pad, n)| !same(n, &g.net) && self.layers.iter().any(|l| pad.on(*l) && self.on(g, *l)) && pad.cu().gap(&me) < self.cl(n, &g.net) - 1e-6)
    }

    /// Something that can't be pushed in the way of a pushed item: a pad, hole, keepout or the
    /// board edge.
    fn fixed_hit(&self, a: P, b: P, g: &Geom) -> bool {
        if self.pad_hit(a, b, g) {
            return true;
        }
        let b0 = &self.p.board;
        let cl = self.cl(&g.net, "");
        if b0.holes.iter().any(|h| pcb::seg_point(a, b, h.at) < h.d / 2.0 + g.r + cl - 1e-6) {
            return true;
        }
        let o = &b0.outline;
        if o.len() >= 3 {
            let edge = b0.rules.edge + g.r;
            if !pcb::inside(o, a) || !pcb::inside(o, b) || (0..o.len()).any(|i| pcb::seg_seg(a, b, o[i], o[(i + 1) % o.len()]) < edge - 1e-6) {
                return true;
            }
        }
        b0.keepouts.iter().any(|k| {
            let applies = if g.via.is_some() { k.vias } else { k.tracks && k.on(g.layer) };
            let n = k.outline.len();
            applies && n >= 3 && (pcb::inside(&k.outline, a) || pcb::inside(&k.outline, b) || (0..n).any(|i| pcb::seg_seg(a, b, k.outline[i], k.outline[(i + 1) % n]) < g.r - 1e-6))
        })
    }

    /// The chain of tracks `j` belongs to: joined end to end, one width, one layer, through
    /// corners that have no via, pad or branch. Returns the track indices and the points.
    fn assemble(&self, j: usize) -> (Vec<usize>, Vec<P>) {
        let t0 = &self.tracks[j];
        let mut ids = vec![j];
        let mut pts = vec![t0.a, t0.b];
        for forward in [true, false] {
            loop {
                let end = if forward { *pts.last().unwrap() } else { pts[0] };
                let last = if forward { *ids.last().unwrap() } else { ids[0] };
                let Some(k) = self.next(last, end, t0) else { break };
                if ids.contains(&k) {
                    break;
                }
                let t = &self.tracks[k];
                let far = if pcb::dist(t.a, end) < 1e-6 { t.b } else { t.a };
                if forward {
                    ids.push(k);
                    pts.push(far);
                } else {
                    ids.insert(0, k);
                    pts.insert(0, far);
                }
            }
        }
        (ids, pts)
    }

    /// The one track that carries on from `from` at `at`, when the corner is a plain one.
    fn next(&self, from: usize, at: P, t0: &Track) -> Option<usize> {
        let touch = |t: &Track| pcb::dist(t.a, at) < 1e-6 || pcb::dist(t.b, at) < 1e-6;
        let mut found = None;
        for (i, t) in self.tracks.iter().enumerate() {
            if i == from || !self.alive[i] || t.net != t0.net || t.layer != t0.layer || !touch(t) {
                continue;
            }
            if found.is_some() || (t.width - t0.width).abs() > 1e-9 {
                return None;
            }
            found = Some(i);
        }
        let via = self.vias.iter().any(|v| v.net == t0.net && pcb::dist(v.at, at) < 1e-6);
        let pad = self.pads.iter().any(|p| p.on(t0.layer) && p.cu().contains(at));
        if via || pad {
            return None;
        }
        found
    }

    /// Swap tracks `old` for the chain `pts`; the new tracks' indices.
    fn replace(&mut self, old: &[usize], pts: &[P], width: f64, layer: Layer, net: &str, rank: i32) -> Vec<usize> {
        for &i in old {
            self.alive[i] = false;
        }
        let mut ids = vec![];
        for s in pts.windows(2).filter(|s| pcb::dist(s[0], s[1]) > 1e-9) {
            ids.push(self.tracks.len());
            self.tracks.push(Track { a: s[0], b: s[1], width, layer, net: net.to_string() });
            self.alive.push(true);
            self.trank.push(rank);
        }
        ids
    }

    /// Move via `v` out of `g`'s way by the shortest way, dragging the tracks that end on it.
    /// Returns the dragged chains.
    fn push_via(&mut self, v: usize, g: &Geom, rank: i32) -> Option<Vec<Vec<usize>>> {
        let at = self.vias[v].at;
        // the nearest point of the pusher to the via
        let (mut q, mut best) = (g.origin, f64::INFINITY);
        for &(a, b) in &g.segs {
            let t = param(a, b, at);
            let c = lerp(a, b, t);
            let d = pcb::dist(c, at);
            if d < best {
                (q, best) = (c, d);
            }
        }
        let need = g.r + self.vias[v].d / 2.0 + self.cl(&g.net, &self.vias[v].net) + EPS;
        let dir = if best > 1e-6 {
            [(at[0] - q[0]) / best, (at[1] - q[1]) / best]
        } else {
            // dead centre on the track: off to its left
            let (a, b) = g.segs[0];
            let l = pcb::dist(a, b).max(1e-9);
            [-(b[1] - a[1]) / l, (b[0] - a[0]) / l]
        };
        let to = [q[0] + dir[0] * need, q[1] + dir[1] * need];
        self.vias[v].at = to;
        self.vrank[v] = rank;
        let net = self.vias[v].net.clone();
        let ends: Vec<usize> = (0..self.tracks.len()).filter(|&i| self.alive[i] && self.tracks[i].net == net && (pcb::dist(self.tracks[i].a, at) < 1e-6 || pcb::dist(self.tracks[i].b, at) < 1e-6)).collect();
        let mut out = vec![];
        for i in ends {
            if !self.alive[i] {
                continue;
            }
            // assemble with the via still at its old place, so the chain stops there
            self.vias[v].at = at;
            let (ids, mut pts) = self.assemble(i);
            self.vias[v].at = to;
            if ids.iter().any(|&k| self.trank[k] >= rank + 1) {
                return None;
            }
            if pcb::dist(pts[0], at) < 1e-6 {
                pts.reverse();
            }
            if pcb::dist(*pts.last().unwrap(), at) > 1e-6 {
                continue;
            }
            pts.pop();
            let from = *pts.last().unwrap();
            let t = self.tracks[i].clone();
            // the bend next to the via (the rest of the track stays put) unless that runs into
            // the pusher, then the bend at the far end
            let reach = g.r + t.width / 2.0 + self.cl(&g.net, &t.net);
            let clear = |path: &[P]| path.windows(2).all(|s| g.segs.iter().all(|&(a, b)| pcb::seg_seg(a, b, s[0], s[1]) >= reach - 1e-6));
            let near_via = bend45(from, to, false);
            let bend = if clear(&near_via) { near_via } else { bend45(from, to, true) };
            pts.extend(bend.into_iter().skip(1));
            out.push(self.replace(&ids, &simplify(&pts), t.width, t.layer, &t.net, rank));
        }
        Some(out)
    }

    /// Whether a chain on `layer` of `net` at half-width `r` is clear of everything except the
    /// tracks `own` (for straightening pushed tracks).
    fn clear_path(&self, pts: &[P], r: f64, layer: Layer, net: &str, own: &[usize]) -> bool {
        let g = Geom { segs: vec![], r, layer: Some(layer), via: None, net: net.to_string(), origin: pts[0] };
        pts.windows(2).all(|s| {
            let (a, b) = (s[0], s[1]);
            let tracks_ok = self.tracks.iter().enumerate().all(|(i, t)| !self.alive[i] || own.contains(&i) || same(&t.net, net) || t.layer != layer || pcb::seg_seg(a, b, t.a, t.b) >= r + t.width / 2.0 + self.cl(net, &t.net) - 1e-6);
            let vias_ok = self.vias.iter().all(|v| same(&v.net, net) || !v.on(layer) || pcb::seg_point(a, b, v.at) >= r + v.d / 2.0 + self.cl(net, &v.net) - 1e-6);
            let head_ok = layer != self.head_layer || same(&self.head_net, net) || self.head.windows(2).all(|h| pcb::seg_seg(a, b, h[0], h[1]) >= r + self.head_w / 2.0 + self.cl(net, &self.head_net) - 1e-6);
            tracks_ok && vias_ok && head_ok && !self.fixed_hit(a, b, &g)
        })
    }
}

/// Re-walk the chain `obs` round the hulls of `g`'s segments (KiCad's ShoveObstacleLine and
/// shoveLineToHullSet): both ways round, keeping the result that is clear of `g`, pushed away
/// from where `g` came from, and shortest; then straightened where the way is clear.
#[allow(clippy::too_many_arguments)]
fn shove_line(w: &World, g: &Geom, obs: &[P], width: f64, layer: Layer, net: &str, cl: f64, own: &[usize]) -> Option<Vec<P>> {
    let grow = cl + width / 2.0 + EPS;
    let reach = g.r + width / 2.0 + cl;
    let near = |a: P, b: P| obs.windows(2).any(|s| pcb::seg_seg(a, b, s[0], s[1]) < reach + width);
    let hulls: Vec<Vec<P>> = if g.via.is_some() {
        vec![octagon(g.origin, g.r + grow)]
    } else {
        g.segs.iter().filter(|(a, b)| near(*a, *b)).map(|&(a, b)| seg_hull(a, b, g.r, grow)).collect()
    };
    if hulls.is_empty() {
        return Some(obs.to_vec());
    }
    let clear_of_g = |pts: &[P]| pts.windows(2).all(|s| g.segs.iter().all(|&(a, b)| pcb::seg_seg(a, b, s[0], s[1]) >= reach - 1e-6));
    let mut best: Option<(f64, Vec<P>)> = None;
    for cw in [false, true] {
        let mut tries = vec![];
        // hull by hull, as KiCad does, then round all of them at once
        let mut path = obs.to_vec();
        let mut ok = true;
        for h in &hulls {
            match walkaround(h, &path, cw) {
                Some(p) => path = simplify(&p),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            tries.push(path);
        }
        if hulls.len() > 1 {
            let all: Vec<P> = hulls.iter().flatten().copied().collect();
            if let Some(p) = walkaround(&convex_hull(&all), obs, cw) {
                tries.push(simplify(&p));
            }
        }
        for path in tries {
            let ends = pcb::dist(path[0], obs[0]) < 1e-6 && pcb::dist(*path.last().unwrap(), *obs.last().unwrap()) < 1e-6;
            if !ends || !clear_of_g(&path) || self_crossing(&path) || !away(obs, &path, g.origin) {
                continue;
            }
            let len = length(&path);
            if best.as_ref().is_none_or(|b| len < b.0) {
                best = Some((len, path));
            }
        }
    }
    let path = best?.1;
    Some(straighten(w, obs, path, width / 2.0, layer, net, own))
}

/// KiCad's checkShoveDirection: the pushed line went away from `from`, not round it.
fn away(old: &[P], new: &[P], from: P) -> bool {
    let mut poly: Vec<P> = old.to_vec();
    poly.extend(new.iter().rev());
    !pcb::inside(&poly, from)
}

/// Cut corners of a pushed chain with 45° bends wherever the way is clear and no longer (a
/// light version of KiCad's optimizer, which tidies shoved lines). Only the stretch that moved
/// is touched (it starts and ends on the old line), so a push stays a local jog.
fn straighten(w: &World, old: &[P], mut pts: Vec<P>, r: f64, layer: Layer, net: &str, own: &[usize]) -> Vec<P> {
    let same_pt = |a: P, b: P| pcb::dist(a, b) < 1e-7;
    let lo = (0..pts.len().min(old.len())).find(|&k| !same_pt(pts[k], old[k])).unwrap_or(pts.len());
    let tail = (0..pts.len().min(old.len())).find(|&k| !same_pt(pts[pts.len() - 1 - k], old[old.len() - 1 - k])).unwrap_or(pts.len());
    // the moved stretch is pts[lo..=hi]; the `tail` points after it stay as they are
    let keep_tail = tail;
    let mut i = lo;
    while i + 2 < pts.len() {
        let hi = pts.len() - 1 - keep_tail.min(pts.len() - 1);
        if i >= hi {
            break;
        }
        let mut done = false;
        for j in (i + 2..=hi.max(i + 2).min(pts.len() - 1)).rev() {
            let old = length(&pts[i..=j]);
            for diag_first in [true, false] {
                let cand = bend45(pts[i], pts[j], diag_first);
                if length(&cand) <= old + 1e-9 && cand.len() < j - i + 1 && w.clear_path(&cand, r, layer, net, own) {
                    let mut v = pts[..i].to_vec();
                    v.extend(cand);
                    v.extend_from_slice(&pts[j + 1..]);
                    pts = simplify(&v);
                    done = true;
                    break;
                }
            }
            if done {
                break;
            }
        }
        if !done {
            i += 1;
        }
    }
    pts
}

// ---------------------------------------------------------------- geometry

fn lerp(a: P, b: P, t: f64) -> P {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Where along a..b (0 to 1) the point nearest `q` is.
fn param(a: P, b: P, q: P) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let l2 = d[0] * d[0] + d[1] * d[1];
    if l2 < 1e-18 {
        return 0.0;
    }
    (((q[0] - a[0]) * d[0] + (q[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0)
}

/// The point of c..d nearest a..b (near enough to order collisions along a..b).
fn closer(a: P, b: P, c: P, d: P) -> P {
    [c, d, lerp(c, d, 0.5), lerp(a, b, param(a, b, c)), lerp(a, b, param(a, b, d))]
        .into_iter()
        .min_by(|x, y| pcb::seg_point(a, b, *x).min(pcb::seg_point(c, d, *x)).total_cmp(&pcb::seg_point(a, b, *y).min(pcb::seg_point(c, d, *y))))
        .unwrap()
}

fn length(pts: &[P]) -> f64 {
    pts.windows(2).map(|s| pcb::dist(s[0], s[1])).sum()
}

/// Drop repeated points and corners that don't turn.
pub fn simplify(pts: &[P]) -> Vec<P> {
    let mut v: Vec<P> = vec![];
    for &p in pts {
        if v.last().is_some_and(|q| pcb::dist(*q, p) < 1e-7) {
            continue;
        }
        if v.len() >= 2 {
            let (a, b) = (v[v.len() - 2], v[v.len() - 1]);
            let cross = (b[0] - a[0]) * (p[1] - b[1]) - (b[1] - a[1]) * (p[0] - b[0]);
            let dot = (b[0] - a[0]) * (p[0] - b[0]) + (b[1] - a[1]) * (p[1] - b[1]);
            if cross.abs() < 1e-9 * (1.0 + pcb::dist(a, b) * pcb::dist(b, p)) && dot > 0.0 {
                v.pop();
            }
        }
        v.push(p);
    }
    v
}

/// The 45° two-segment path from a to b, diagonal first or straight first.
pub fn bend45(a: P, b: P, diag_first: bool) -> Vec<P> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let m = dx.abs().min(dy.abs());
    let corner = if diag_first { [a[0] + m * dx.signum(), a[1] + m * dy.signum()] } else { [b[0] - m * dx.signum(), b[1] - m * dy.signum()] };
    simplify(&[a, corner, b])
}

fn self_crossing(pts: &[P]) -> bool {
    let n = pts.len();
    (0..n.saturating_sub(1)).any(|i| (i + 2..n - 1).any(|j| pcb::segs_cross(pts[i], pts[i + 1], pts[j], pts[j + 1])))
}

/// Make a polygon anticlockwise (in maths axes).
fn ccw(mut poly: Vec<P>) -> Vec<P> {
    let area: f64 = (0..poly.len()).map(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        a[0] * b[1] - b[0] * a[1]
    }).sum();
    if area < 0.0 {
        poly.reverse();
    }
    poly
}

/// KiCad's OctagonalHull for a round thing: an octagon whose sides are `h` from `c`.
fn octagon(c: P, h: f64) -> Vec<P> {
    let k = (2.0 - std::f64::consts::SQRT_2) * h;
    ccw(vec![
        [c[0] - h, c[1] - h + k],
        [c[0] - h + k, c[1] - h],
        [c[0] + h - k, c[1] - h],
        [c[0] + h, c[1] - h + k],
        [c[0] + h, c[1] + h - k],
        [c[0] + h - k, c[1] + h],
        [c[0] - h + k, c[1] + h],
        [c[0] - h, c[1] + h - k],
    ])
}

/// KiCad's SegmentHull: the octagon round a track a..b of half-width `r`, `grow` further out.
fn seg_hull(a: P, b: P, r: f64, grow: f64) -> Vec<P> {
    let d = r + grow;
    let len = pcb::dist(a, b);
    if len < 1e-9 {
        return octagon(a, d);
    }
    let x2 = d / (1.0 + std::f64::consts::SQRT_2);
    let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
    let n = [-u[1], u[0]];
    let s = |o: P, f: &[(P, f64)]| f.iter().fold(o, |acc, (v, k)| [acc[0] + v[0] * k, acc[1] + v[1] * k]);
    ccw(vec![
        s(b, &[(n, d), (u, x2)]),
        s(b, &[(u, d), (n, x2)]),
        s(b, &[(u, d), (n, -x2)]),
        s(b, &[(n, -d), (u, x2)]),
        s(a, &[(n, -d), (u, -x2)]),
        s(a, &[(u, -d), (n, -x2)]),
        s(a, &[(u, -d), (n, x2)]),
        s(a, &[(n, d), (u, -x2)]),
    ])
}

/// Anticlockwise convex hull (monotone chain).
fn convex_hull(pts: &[P]) -> Vec<P> {
    let mut p = pts.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup_by(|a, b| pcb::dist(*a, *b) < 1e-12);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: P, a: P, b: P| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<P> = vec![];
    for &q in &p {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], q) <= 0.0 {
            lower.pop();
        }
        lower.push(q);
    }
    let mut upper: Vec<P> = vec![];
    for &q in p.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], q) <= 0.0 {
            upper.pop();
        }
        upper.push(q);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Strictly inside a convex anticlockwise polygon.
fn inside_convex(h: &[P], q: P) -> bool {
    (0..h.len()).all(|i| {
        let (a, b) = (h[i], h[(i + 1) % h.len()]);
        (b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0]) > 1e-9
    })
}

/// Segment-segment intersection parameters (touching counts).
fn hit(a: P, b: P, c: P, d: P) -> Option<(f64, f64)> {
    let r = [b[0] - a[0], b[1] - a[1]];
    let s = [d[0] - c[0], d[1] - c[1]];
    let den = r[0] * s[1] - r[1] * s[0];
    if den.abs() < 1e-15 {
        return None;
    }
    let qp = [c[0] - a[0], c[1] - a[1]];
    let t = (qp[0] * s[1] - qp[1] * s[0]) / den;
    let u = (qp[0] * r[1] - qp[1] * r[0]) / den;
    ((-1e-12..=1.0 + 1e-12).contains(&t) && (-1e-12..=1.0 + 1e-12).contains(&u)).then_some((t.clamp(0.0, 1.0), u.clamp(0.0, 1.0)))
}

/// KiCad's LINE::Walkaround for a convex hull: the path with the stretch from where it first
/// enters the hull to where it last leaves replaced by the hull's outline, going anticlockwise
/// (`cw` false) or clockwise round it. None when an end of the path is inside the hull.
fn walkaround(hull: &[P], path: &[P], cw: bool) -> Option<Vec<P>> {
    let n = hull.len();
    if n < 3 || path.len() < 2 {
        return None;
    }
    if inside_convex(hull, path[0]) || inside_convex(hull, *path.last().unwrap()) {
        return None;
    }
    // (position along the path, hull edge, position along the edge, point)
    let mut ips: Vec<(f64, usize, f64, P)> = vec![];
    for (k, s) in path.windows(2).enumerate() {
        for e in 0..n {
            if let Some((t, u)) = hit(s[0], s[1], hull[e], hull[(e + 1) % n]) {
                ips.push((k as f64 + t, e, u, lerp(s[0], s[1], t)));
            }
        }
    }
    // a path that only touches the hull, or misses it, stays as it is
    let enters = path.windows(2).any(|s| inside_convex(hull, lerp(s[0], s[1], 0.5))) || path.iter().any(|q| inside_convex(hull, *q)) || ips.windows(2).any(|w| inside_convex(hull, lerp(w[0].3, w[1].3, 0.5)));
    if ips.len() < 2 || !enters {
        return Some(path.to_vec());
    }
    let first = ips.iter().min_by(|a, b| a.0.total_cmp(&b.0)).copied().unwrap();
    let last = ips.iter().max_by(|a, b| a.0.total_cmp(&b.0)).copied().unwrap();
    let mut out: Vec<P> = path[..=first.0.floor().min((path.len() - 2) as f64) as usize].to_vec();
    out.push(first.3);
    // round the hull from the entry edge to the exit edge
    let (e1, s1, e2, s2) = (first.1, first.2, last.1, last.2);
    if !cw {
        // anticlockwise: vertices e1+1, e1+2, …, e2
        if !(e1 == e2 && s2 >= s1) {
            let mut e = e1;
            loop {
                e = (e + 1) % n;
                out.push(hull[e]);
                if e == e2 {
                    break;
                }
            }
        }
    } else if !(e1 == e2 && s2 <= s1) {
        // clockwise: vertices e1, e1-1, …, e2+1
        let mut e = e1;
        loop {
            out.push(hull[e]);
            if e == (e2 + 1) % n {
                break;
            }
            e = (e + n - 1) % n;
        }
    }
    out.push(last.3);
    let k = last.0.floor().min((path.len() - 2) as f64) as usize;
    out.extend_from_slice(&path[k + 1..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Pcb {
        let mut p = Pcb::new("t");
        p.board.outline = vec![[0.0, 0.0], [40.0, 0.0], [40.0, 30.0], [0.0, 30.0]];
        p
    }

    fn clear_of(tracks: &[Track], head: &[P], w: f64, cl: f64) -> bool {
        tracks.iter().all(|t| head.windows(2).all(|s| pcb::seg_seg(s[0], s[1], t.a, t.b) >= w / 2.0 + t.width / 2.0 + cl - 1e-6))
    }

    #[test]
    fn walkaround_goes_round_a_hull_both_ways() {
        let h = octagon([5.0, 0.0], 1.0);
        let path = [[0.0, 0.0], [10.0, 0.0]];
        let a = walkaround(&h, &path, false).unwrap();
        let b = walkaround(&h, &path, true).unwrap();
        assert_eq!(a[0], path[0]);
        assert_eq!(*a.last().unwrap(), path[1]);
        // one way passes below (y -1), the other above (y +1), and neither enters the hull
        let ys = |p: &[P]| p.iter().map(|q| q[1]).fold(0.0f64, |m, y| if y.abs() > m.abs() { y } else { m });
        assert!((ys(&a) + ys(&b)).abs() < 1e-9 && (ys(&a).abs() - 1.0).abs() < 1e-9, "{a:?} {b:?}");
        for p in [&a, &b] {
            assert!(p.windows(2).all(|s| !inside_convex(&h, lerp(s[0], s[1], 0.5))));
        }
        // a path that misses the hull is left alone
        assert_eq!(walkaround(&h, &[[0.0, 5.0], [10.0, 5.0]], false).unwrap(), vec![[0.0, 5.0], [10.0, 5.0]]);
    }

    #[test]
    fn a_track_across_the_route_is_pushed_aside() {
        let mut p = board();
        // B runs left to right at y = 10; the new track A comes up from below to y = 9.8
        p.board.tracks.push(Track { a: [5.0, 10.0], b: [35.0, 10.0], width: 0.25, layer: Layer::Top, net: "B".into() });
        let head = [[20.0, 2.0], [20.0, 9.8]];
        let s = shove(&p, &[], &[], &Head { pts: &head, width: 0.25, layer: Layer::Top, net: "A" }).expect("shoved");
        let cl = p.board.rules.clearance;
        assert!(s.moved > 0);
        assert!(clear_of(&s.tracks, &head, 0.25, cl), "{:?}", s.tracks);
        // B still runs from end to end, pushed up (away from where A came from)
        let pts: Vec<P> = s.tracks.iter().flat_map(|t| [t.a, t.b]).collect();
        assert!(pts.iter().any(|q| *q == [5.0, 10.0]) && pts.iter().any(|q| *q == [35.0, 10.0]));
        assert!(pts.iter().all(|q| q[1] >= 10.0 - 1e-9), "{pts:?}");
        // and it is all 0/45/90 degrees
        for t in &s.tracks {
            let (dx, dy) = ((t.b[0] - t.a[0]).abs(), (t.b[1] - t.a[1]).abs());
            assert!(dx < 1e-6 || dy < 1e-6 || (dx - dy).abs() < 1e-6, "{t:?}");
        }
    }

    #[test]
    fn pushes_carry_on_to_the_next_track() {
        let mut p = board();
        p.board.tracks.push(Track { a: [5.0, 10.0], b: [35.0, 10.0], width: 0.25, layer: Layer::Top, net: "B".into() });
        p.board.tracks.push(Track { a: [5.0, 10.5], b: [35.0, 10.5], width: 0.25, layer: Layer::Top, net: "C".into() });
        let head = [[20.0, 2.0], [20.0, 9.8]];
        let s = shove(&p, &[], &[], &Head { pts: &head, width: 0.25, layer: Layer::Top, net: "A" }).expect("shoved");
        let cl = p.board.rules.clearance;
        assert!(clear_of(&s.tracks, &head, 0.25, cl));
        // B and C keep their clearance to each other
        let (b, c): (Vec<&Track>, Vec<&Track>) = s.tracks.iter().partition(|t| t.net == "B");
        for x in &b {
            for y in &c {
                assert!(pcb::seg_seg(x.a, x.b, y.a, y.b) >= 0.25 + cl - 1e-6, "{x:?} {y:?}");
            }
        }
    }

    #[test]
    fn a_via_is_pushed_and_drags_its_track() {
        let mut p = board();
        p.board.tracks.push(Track { a: [10.0, 10.0], b: [20.0, 10.0], width: 0.25, layer: Layer::Top, net: "B".into() });
        p.board.vias.push(Via::new([20.0, 10.0], 0.6, 0.3, "B"));
        // A comes up from below, ending just short of B's via
        let head = [[20.2, 2.0], [20.2, 9.5]];
        let s = shove(&p, &[], &[], &Head { pts: &head, width: 0.25, layer: Layer::Top, net: "A" }).expect("shoved");
        let v = s.vias[0].at;
        let cl = p.board.rules.clearance;
        assert!(pcb::seg_point(head[0], head[1], v) >= 0.3 + 0.125 + cl - 1e-6, "{v:?}");
        // the track still ends on the via
        assert!(s.tracks.iter().any(|t| pcb::dist(t.a, v) < 1e-6 || pcb::dist(t.b, v) < 1e-6));
        assert!(clear_of(&s.tracks, &head, 0.25, cl));
    }

    #[test]
    fn the_board_edge_stops_the_shove() {
        let mut p = board();
        // B can't move: it is squeezed between the route and the board edge
        p.board.tracks.push(Track { a: [5.0, 29.5], b: [35.0, 29.5], width: 0.25, layer: Layer::Top, net: "B".into() });
        let head = [[20.0, 2.0], [20.0, 29.3]];
        assert!(shove(&p, &[], &[], &Head { pts: &head, width: 0.25, layer: Layer::Top, net: "A" }).is_none());
    }
}
