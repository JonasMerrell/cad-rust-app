//! Feed scheduling: keep the toolpath's geometry and set each cutting move's feed from the cut the stock
//! simulation measures there, so the spindle works at a target power all along the path instead of the
//! programmed feed being set for the worst spot (feedrate scheduling, as in Erdim, Lazoglu & Ozturk 2006;
//! the cutting model is `cam_cutting`, Kienzle with mc = 0.25 for aluminium, so the power goes with
//! feed^(1 − mc)).
//!
//! One implementation for two uses:
//! - `feed_schedule` (op or setup): roughing moves go UP or DOWN to the target, between a floor (0.3 × the
//!   programmed feed, and a 0.02 mm chip per tooth) and a ceiling (`feed_max_scale` × the programmed feed and
//!   the machine's top feed); air moves in cleared stock run at the ceiling.
//! - `power_target` alone (adaptive roughing from the power budget, `cam_paths::power_budget`): feeds only
//!   come DOWN, to the budget's cap (the spindle's caution at that speed, or `power_cap`), never under a
//!   quarter of the programmed feed.
//!
//! The schedule's target and cap are the power budget's too (60 % of the power the spindle has at the tool's
//! speed, cap at its caution: 671 / 850 W at 9000 on the DMC2), from the op's `power_target` / `power_cap`,
//! else the setup's `feed_target`.
//!
//! Both: long moves are split (about 2 mm pieces) so the feed can change along them; a move takes the lowest
//! feed any cut within a tool radius ahead of it needs (the tool slows before it reaches a wall, not in it);
//! the force, bend, stress, torque and chip stay inside the tool's and machine's limits; feeds are rounded
//! down to steps of at most 5 % and pieces of one move at one feed joined again (arc fitting still finds its
//! runs); then the whole setup is simulated again and any move still over is lowered (up to 3 rounds).

use crate::cam::{Cam, OpKind, Operation};
use crate::cam_cutting::{self as cc, Limits, Material, ToolGeom};
use crate::cam_paths::{stock_box, Move, Part, Toolpath};
use crate::cam_stock::StockMap;
use crate::cam_sim::{LoadSample, Sim};
use glam::DVec3;

/// Longest piece a cutting move is split into, mm.
const SPLIT: f64 = 2.0;
/// The lowest chip per tooth the schedule slows to (below it the edge rubs), mm.
const MIN_CHIP: f64 = 0.02;
/// Shorter runs at a higher feed than a neighbour take the neighbour's (fewer feed changes), mm.
const MIN_RUN: f64 = 3.0;

/// How one toolpath's feeds are set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// Power to aim at, W, and the most any move may take after scheduling.
    pub target: f64,
    pub cap: f64,
    /// Raise feeds where the cut is light (the schedule), or only lower them (the power budget's cap).
    pub raise: bool,
    /// Ceiling and floor as shares of each move's programmed feed; the machine's top feed (0 = none);
    /// the lowest chip per tooth.
    pub max_scale: f64,
    pub min_scale: f64,
    pub max_feed: f64,
    pub min_chip: f64,
    /// How far ahead a move looks for heavier cuts, mm.
    pub lookahead: f64,
    /// For the report: the note's name.
    pub budget: bool,
    /// The op's cutting feed: moves programmed slower (ramps, helices and plunges at the plunge feed, lead-ins)
    /// keep their feed and don't hold back the moves before them.
    pub cut_feed: f64,
}

/// What the schedule did to one operation (for simulate_cam and the setup sheet).
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub target_w: f64,
    pub cap_w: f64,
    /// Whole toolpath time, s, before and after.
    pub time_before_s: f64,
    pub time_after_s: f64,
    /// Cutting time (moves that take material), s, before and after.
    pub cut_before_s: f64,
    pub cut_after_s: f64,
    /// Highest and mean (time-weighted while cutting) power, W.
    pub peak_before_w: f64,
    pub peak_after_w: f64,
    pub mean_before_w: f64,
    pub mean_after_w: f64,
    /// Share of the cutting time at the feed ceiling (the machine's top feed or the max scale), %: there the
    /// feed can't go up any more and a wider stepover (not a faster feed) is the next lever.
    pub at_ceiling_pct: f64,
    /// Share of the cutting time held under the target by force / bend / stress / chip, or sitting at the floor.
    pub limited_pct: f64,
    pub at_floor_pct: f64,
    /// Rounds of re-simulation it took, and feed moves before and after.
    pub rounds: u32,
    pub moves_before: usize,
    pub moves_after: usize,
}

impl Report {
    pub fn note(&self) -> String {
        format!(
            "feed schedule: {} W target; {} → {} s; peak {} → {} W, mean while cutting {} → {} W; {}% of the cut at the feed ceiling{}",
            self.target_w.round(),
            self.time_before_s.round(),
            self.time_after_s.round(),
            self.peak_before_w.round(),
            self.peak_after_w.round(),
            self.mean_before_w.round(),
            self.mean_after_w.round(),
            self.at_ceiling_pct.round(),
            if self.at_ceiling_pct > 30.0 { " (a wider stepover is the next lever there)" } else { "" }
        )
    }
}

/// The default target at `rpm`: the power budget's (`cam_paths::power_budget`: 60 % of the power the spindle
/// has at that speed, no more than its caution), or the setup's `feed_target`.
#[cfg(test)]
pub fn default_target(cam: &Cam, rpm: f64) -> f64 {
    let op = Operation { power_target: Some(cam.feed_target.unwrap_or(0.0)), ..Default::default() };
    let tool = crate::cam::Tool { rpm, ..Default::default() };
    crate::cam_paths::power_budget(cam, &op, &tool).map(|b| b.0).unwrap_or(0.0)
}

/// Roughing operations the schedule may change (finishing keeps its constant feed).
pub fn roughing(op: &Operation) -> bool {
    match op.kind {
        OpKind::Adaptive | OpKind::Pocket | OpKind::Clear | OpKind::Face => true,
        OpKind::Contour => op.leave > 0.0,
        _ => false,
    }
}

/// Whether, and how, a toolpath's feeds are set.
pub fn plan_for(cam: &Cam, tp: &Toolpath) -> Option<Plan> {
    let op = cam.op(tp.op)?;
    let tool = cam.path_tool(tp.op, tp.tool)?;
    let scheduled = op.feed_schedule.unwrap_or(cam.feed_schedule) && roughing(op);
    let max_feed = cam.machine.max_feed.max(0.0);
    let lookahead = tool.radius().max(1.0);
    let cut_feed = cam.feeds(&tool).0;
    if scheduled {
        // the target and cap of the power budget, the setup's feed_target standing in for an op without one
        let mut o = op.clone();
        o.power_target = o.power_target.or(Some(cam.feed_target.unwrap_or(0.0)));
        let (target, cap) = crate::cam_paths::power_budget(cam, &o, &tool)?;
        let max_scale = op.feed_max_scale.filter(|s| *s > 0.0).unwrap_or(2.0);
        return Some(Plan { target, cap, raise: true, max_scale, min_scale: 0.3, max_feed, min_chip: MIN_CHIP, lookahead, budget: op.power_target.is_some(), cut_feed });
    }
    let (target, cap) = crate::cam_paths::power_budget(cam, op, &tool)?;
    let _ = target;
    Some(Plan { target: cap, cap, raise: false, max_scale: 1.0, min_scale: 0.25, max_feed, min_chip: 0.0, lookahead, budget: true, cut_feed })
}

/// Set the feeds of every toolpath that asks for it (see the module notes). `stock_in`: what an earlier setup left.
pub fn apply(cam: &Cam, part: &Part, stock_in: Option<&StockMap>, mut paths: Vec<Toolpath>) -> Vec<Toolpath> {
    let plans: Vec<Option<Plan>> = paths.iter().map(|tp| plan_for(cam, tp)).collect();
    if plans.iter().all(|p| p.is_none()) {
        return paths;
    }
    // split; `origin[i][k]` = the move piece k of toolpath i came from (to join them again)
    let mut origin: Vec<Vec<usize>> = vec![];
    let mut moves_before = vec![0usize; paths.len()];
    for (i, tp) in paths.iter_mut().enumerate() {
        moves_before[i] = tp.moves.len();
        if plans[i].is_some() {
            let (m, o) = split(&tp.moves);
            tp.moves = m;
            origin.push(o);
        } else {
            origin.push((0..tp.moves.len()).collect());
        }
    }
    let programmed: Vec<Vec<f64>> = paths.iter().map(|tp| tp.moves.iter().map(|m| if let Move::Feed(_, f) = m { *f } else { 0.0 }).collect()).collect();
    let before = paths.clone();
    let (prof0, model) = profile(cam, part, stock_in, &paths);
    let bases = bases(&paths);
    let (material, limits) = model.0;
    let geoms = &model.1;
    // the stock's edge falls a cell either way on another grid (the export's own simulation): on a cut a
    // couple of mm wide that is ±10 % of its width, so plan for half a cell wider
    let margin = 0.5 * model.2;
    // what each piece may take: the lowest any of its load samples needs (the geometry doesn't change with the
    // feed, so the first simulation's samples hold for every round)
    let mut need: Vec<Vec<f64>> = paths.iter().map(|tp| vec![f64::INFINITY; tp.moves.len()]).collect();
    let mut limited: Vec<Vec<bool>> = paths.iter().map(|tp| vec![false; tp.moves.len()]).collect();
    let mut cutting: Vec<Vec<bool>> = paths.iter().map(|tp| vec![false; tp.moves.len()]).collect();
    for s in &prof0 {
        let Some((i, k)) = locate(&bases, s.step) else { continue };
        if s.area > 1e-3 {
            cutting[i][k] = true;
        }
        let (Some(plan), Some((g, rpm, finishing))) = (plans[i], geoms.get(i).copied().flatten()) else { continue };
        if finishing || s.area <= 1e-3 {
            continue;
        }
        let prog = programmed[i][k];
        let (lo, hi) = bounds(&plan, prog, &g, rpm);
        let wide = LoadSample { width: s.width + margin, area: s.area + margin * s.depth, ..*s };
        let (f, lim) = feed_for(&plan, &g, &material, &limits, rpm, &wide, lo, hi);
        if f < need[i][k] {
            need[i][k] = f;
            limited[i][k] = lim;
        }
    }
    let mut rounds = 0;
    let mut extra: Vec<Vec<f64>> = paths.iter().map(|tp| vec![1.0; tp.moves.len()]).collect();
    loop {
        rounds += 1;
        for (i, tp) in paths.iter_mut().enumerate() {
            let Some(plan) = plans[i] else { continue };
            let Some((g, rpm, _)) = geoms.get(i).copied().flatten() else { continue };
            let feeds = schedule(&plan, &before[i].moves, &programmed[i], &need[i], &extra[i], &g, rpm);
            for (k, m) in tp.moves.iter_mut().enumerate() {
                if let Move::Feed(p, _) = *m {
                    *m = Move::Feed(p, feeds[k]);
                }
            }
        }
        if rounds > 3 {
            break;
        }
        // check: anything over?
        let (prof, _) = profile(cam, part, stock_in, &paths);
        let mut over = false;
        for s in &prof {
            let Some((i, k)) = locate(&bases, s.step) else { continue };
            let Some(plan) = plans[i] else { continue };
            let limit = if plan.raise { (plan.target * 1.02).min(plan.cap) } else { plan.cap };
            let prog = programmed[i][k];
            let Some((g, rpm, _)) = geoms.get(i).copied().flatten() else { continue };
            let floor = bounds(&plan, prog, &g, rpm).0;
            if (s.power > limit || s.worst > 1.0) && s.feed > floor + 0.5 {
                // power ∝ feed^(1 − mc): the share of this feed that brings it back under
                let by = (limit / s.power.max(1e-9)).min(1.0 / s.worst.max(1e-9)).powf(1.0 / (1.0 - material.mc)) * 0.98;
                extra[i][k] = extra[i][k].min(by);
                over = true;
            }
        }
        if !over {
            break;
        }
    }
    // where the cut sits against its bounds (on the pieces), then join the pieces of one move at one feed
    let mut shares = vec![(0.0, 0.0, 0.0, 0.0); paths.len()];
    for (i, tp) in paths.iter_mut().enumerate() {
        let (Some(plan), Some((g, rpm, _))) = (plans[i], geoms.get(i).copied().flatten()) else { continue };
        let (mut all, mut ceil_s, mut lim_s, mut floor_s) = (0.0, 0.0, 0.0, 0.0);
        let mut p = DVec3::ZERO;
        for (k, m) in tp.moves.iter().enumerate() {
            let q = m.end(p);
            if let (Move::Feed(_, f), true) = (m, cutting[i][k] && !fixed(&plan, programmed[i][k], k > 0 && plunging(p, q))) {
                let t = p.distance(q) / f.max(1e-9) * 60.0;
                let (lo, hi) = bounds(&plan, programmed[i][k], &g, rpm);
                all += t;
                if *f >= quantize(hi) - 1e-6 {
                    ceil_s += t;
                } else if *f <= quantize(lo) + 1e-6 {
                    floor_s += t;
                } else if limited[i][k] {
                    lim_s += t;
                }
            }
            p = q;
        }
        let pct = |s: f64| if all > 0.0 { s / all * 100.0 } else { 0.0 };
        shares[i] = (pct(ceil_s), pct(lim_s), pct(floor_s), all);
        tp.moves = join(&tp.moves, &origin[i]);
    }
    // the report, from the moves as they will run
    let (prof1, _) = profile(cam, part, stock_in, &paths);
    let bases1 = self::bases(&paths);
    for (i, tp) in paths.iter_mut().enumerate() {
        let Some(plan) = plans[i] else { continue };
        let (cut0, peak0, mean0) = op_stats(&bases, i, &prof0);
        let (cut1, peak1, mean1) = op_stats(&bases1, i, &prof1);
        let mut rep = Report {
            target_w: plan.target,
            cap_w: plan.cap,
            time_before_s: 0.0,
            time_after_s: 0.0,
            cut_before_s: cut0,
            cut_after_s: cut1,
            peak_before_w: peak0,
            peak_after_w: peak1,
            mean_before_w: mean0,
            mean_after_w: mean1,
            at_ceiling_pct: shares[i].0,
            limited_pct: shares[i].1,
            at_floor_pct: shares[i].2,
            rounds,
            moves_before: moves_before[i],
            moves_after: tp.moves.len(),
        };
        if let Some(tool) = cam.path_tool(tp.op, tp.tool) {
            let mut b = before[i].clone();
            b.moves = join(&b.moves, &origin[i]);
            crate::cam_paths::toolpath_stats(&mut b, &tool, cam);
            crate::cam_paths::toolpath_stats(tp, &tool, cam);
            rep.time_before_s = b.time;
            rep.time_after_s = tp.time;
        }
        if plan.raise {
            tp.warnings.push(rep.note());
        }
        if plan.budget {
            let op = cam.op(tp.op);
            let target = match (op, cam.path_tool(tp.op, tp.tool)) {
                (Some(o), Some(t)) => crate::cam_paths::power_budget(cam, o, &t).map(|b| b.0).unwrap_or(plan.target),
                _ => plan.target,
            };
            tp.warnings.push(format!(
                "power budget: {} W target (cap {}), {} W mean while cutting, {} W at the most",
                target.round(),
                plan.cap.round(),
                mean1.round(),
                peak1.round()
            ));
        }
        tp.feed_report = Some(rep);
    }
    paths
}

/// Each toolpath's first move in the simulation's list.
fn bases(paths: &[Toolpath]) -> Vec<usize> {
    let mut b = Vec::with_capacity(paths.len() + 1);
    let mut n = 0;
    for tp in paths {
        b.push(n);
        n += tp.moves.len();
    }
    b.push(n);
    b
}

/// The toolpath and move of a simulation step.
fn locate(bases: &[usize], step: usize) -> Option<(usize, usize)> {
    let i = bases.partition_point(|b| *b <= step).checked_sub(1)?;
    (i + 1 < bases.len()).then(|| (i, step - bases[i]))
}

type Model = ((Material, Limits), Vec<Option<(ToolGeom, f64, bool)>>, f64);

/// Simulate the whole setup and record every load sample.
fn profile(cam: &Cam, part: &Part, stock_in: Option<&StockMap>, paths: &[Toolpath]) -> (Vec<LoadSample>, Model) {
    let stock = stock_box(cam, part);
    let res = crate::cam_paths::sim_resolution(stock);
    let mut sim = Sim::new(cam, part, stock, paths, res);
    if let Some(m) = stock_in {
        sim.start_from(m);
    }
    sim.profile = Some(vec![]);
    sim.run_to(usize::MAX, usize::MAX);
    let geoms = (0..paths.len()).map(|i| sim.cutter_model(i)).collect();
    (sim.profile.take().unwrap_or_default(), (sim.cut_model(), geoms, sim.cell))
}

/// A plunge or a ramp down steeper than 1 in 2: the end of the tool cuts, which the sideways model doesn't cover.
fn plunging(from: DVec3, to: DVec3) -> bool {
    to.z < from.z - 1e-9 && (to - from).truncate().length() < 2.0 * (from.z - to.z)
}

/// A move the schedule leaves at its programmed feed: a plunge or steep ramp, or anything programmed slower than
/// the op's cutting feed (helix and ramp entries at the plunge feed, lead-ins).
fn fixed(plan: &Plan, prog: f64, plunge: bool) -> bool {
    plunge || prog < 0.98 * plan.cut_feed
}

/// Split long feed moves (not plunges) into pieces of at most `SPLIT` mm.
fn split(moves: &[Move]) -> (Vec<Move>, Vec<usize>) {
    let (mut out, mut origin) = (Vec::with_capacity(moves.len()), Vec::with_capacity(moves.len()));
    let mut p = DVec3::ZERO;
    for (k, m) in moves.iter().enumerate() {
        let q = m.end(p);
        match *m {
            Move::Feed(_, f) if k > 0 && !plunging(p, q) && p.distance(q) > SPLIT => {
                let n = (p.distance(q) / SPLIT).ceil() as usize;
                for j in 1..=n {
                    let at = if j == n { q } else { p + (q - p) * (j as f64 / n as f64) };
                    out.push(Move::Feed(at.to_array(), f));
                    origin.push(k);
                }
            }
            _ => {
                out.push(*m);
                origin.push(k);
            }
        }
        p = q;
    }
    (out, origin)
}

/// Join consecutive pieces of one move that ended at the same feed.
fn join(moves: &[Move], origin: &[usize]) -> Vec<Move> {
    let mut out: Vec<Move> = Vec::with_capacity(moves.len());
    let mut last: Option<usize> = None;
    for (k, m) in moves.iter().enumerate() {
        let o = origin.get(k).copied();
        if let (Move::Feed(p, f), Some(Move::Feed(_, g))) = (m, out.last()) {
            if o.is_some() && o == last && (f - g).abs() < 1e-9 {
                *out.last_mut().unwrap() = Move::Feed(*p, *f);
                continue;
            }
        }
        out.push(*m);
        last = o;
    }
    out
}

/// A move's floor and ceiling.
fn bounds(plan: &Plan, prog: f64, g: &ToolGeom, rpm: f64) -> (f64, f64) {
    let mut hi = prog * plan.max_scale.max(1.0);
    if !plan.raise {
        hi = prog;
    }
    if plan.max_feed > 0.0 {
        hi = hi.min(plan.max_feed.max(prog.min(plan.max_feed)));
    }
    let chip = plan.min_chip * g.flutes.max(1) as f64 * rpm;
    let lo = (prog * plan.min_scale).max(chip).min(prog).min(hi);
    (lo, hi)
}

/// The highest feed in [lo, hi] at which this cut takes no more than the target and stays inside every other
/// limit (force, bend, stress, torque, chip); and whether another limit than the power set it.
#[allow(clippy::too_many_arguments)]
fn feed_for(plan: &Plan, g: &ToolGeom, m: &Material, l: &Limits, rpm: f64, s: &LoadSample, lo: f64, hi: f64) -> (f64, bool) {
    let check = |f: f64| {
        let c = cc::cut(g, m, s.width, s.depth, f, rpm, s.area);
        let u = cc::use_of(&c, g, l, rpm, false);
        (c.power <= plan.target, u.worst <= 0.97)
    };
    let (p, o) = check(hi);
    if p && o {
        return (hi, false);
    }
    let (pl, ol) = check(lo);
    if !(pl && ol) {
        return (lo, !ol);
    }
    let (mut a, mut b) = (lo, hi);
    for _ in 0..30 {
        let mid = 0.5 * (a + b);
        let (p, o) = check(mid);
        if p && o {
            a = mid;
        } else {
            b = mid;
        }
        if b - a < 1.0 {
            break;
        }
    }
    let (pp, _) = check((a + 2.0).min(hi));
    (a, pp)
}

/// Feeds rounded down to steps of at most 5 %: 10 mm/min under 400, 25 under 1000, 50 above.
pub fn quantize(f: f64) -> f64 {
    let step = if f < 400.0 {
        10.0
    } else if f < 1000.0 {
        25.0
    } else {
        50.0
    };
    ((f / step + 1e-9).floor() * step).max(step.min(f))
}

/// One toolpath's new feeds, piece by piece: each piece's own need (the ceiling in air, its programmed feed for
/// a plunge), then the lowest need within `lookahead` mm ahead, rounded down, short fast runs brought down to a
/// neighbour.
#[allow(clippy::too_many_arguments)]
fn schedule(plan: &Plan, moves: &[Move], prog: &[f64], need: &[f64], extra: &[f64], g: &ToolGeom, rpm: f64) -> Vec<f64> {
    let n = moves.len();
    let mut want = vec![f64::INFINITY; n];
    let mut held = vec![false; n];
    let (mut start, mut len) = (vec![0.0; n], vec![0.0; n]);
    let mut p = DVec3::ZERO;
    let mut at = 0.0;
    for (k, m) in moves.iter().enumerate() {
        let q = m.end(p);
        start[k] = at;
        len[k] = if k == 0 { 0.0 } else { p.distance(q) };
        at += len[k];
        if let Move::Feed(..) = m {
            let (lo, hi) = bounds(plan, prog[k], g, rpm);
            if fixed(plan, prog[k], k > 0 && plunging(p, q)) {
                held[k] = true;
                p = q;
                continue;
            }
            want[k] = if need[k].is_finite() {
                need[k]
            } else {
                hi
            };
            want[k] = (want[k] * extra[k]).min(hi).max(lo.min(want[k] * extra[k]));
        }
        p = q;
    }
    // look ahead along the feed moves (a rapid ends the run: the tool left the cut)
    let mut out = prog.to_vec();
    for k in 0..n {
        if !want[k].is_finite() {
            continue;
        }
        let end = start[k] + len[k] + plan.lookahead;
        let mut f = want[k];
        let mut j = k + 1;
        while j < n && (want[j].is_finite() || held[j]) && start[j] < end {
            if !held[j] {
                f = f.min(want[j]);
            }
            j += 1;
        }
        out[k] = quantize(f);
    }
    // short runs faster than a neighbour come down to it
    let mut k = 0;
    while k < n {
        if !want[k].is_finite() {
            k += 1;
            continue;
        }
        let mut e = k;
        let mut l = 0.0;
        while e < n && want[e].is_finite() && out[e] == out[k] {
            l += len[e];
            e += 1;
        }
        if l < MIN_RUN {
            let before = (k > 0 && want[k - 1].is_finite()).then(|| out[k - 1]).filter(|f| *f < out[k]);
            let after = (e < n && want[e].is_finite()).then(|| out[e]).filter(|f| *f < out[k]);
            if let Some(f) = before.into_iter().chain(after).reduce(f64::max) {
                out[k..e].iter_mut().for_each(|o| *o = f);
            }
        }
        k = e;
    }
    out
}

/// Cutting time, highest and time-weighted mean power over toolpath `i`'s cutting, from load samples.
fn op_stats(bases: &[usize], i: usize, prof: &[LoadSample]) -> (f64, f64, f64) {
    let (lo, hi) = (bases[i], bases[i + 1]);
    let (mut t, mut e, mut peak) = (0.0, 0.0, 0.0f64);
    for s in prof.iter().filter(|s| s.step >= lo && s.step < hi && s.area > 1e-3) {
        let dt = s.len / s.feed.max(1e-9) * 60.0;
        t += dt;
        e += s.power * dt;
        peak = peak.max(s.power);
    }
    (t, peak, if t > 0.0 { e / t } else { 0.0 })
}

/// Below this much material per mm of travel (mm²) a feed move counts as air (with no stock within two cells of
/// the tool's side anywhere along it either).
const AIR_AREA: f64 = 0.05;
/// Feed moves longer than this are split so their ends can keep the cutting feed (mm).
const LINK_SPLIT: f64 = 3.0;
/// Length kept at the cutting feed at the end of a link before the cut it leads into, and at its start after
/// one (the load check looks a millimetre back), mm.
const APPROACH: f64 = 1.0;
const LEAVE: f64 = 1.5;

/// G1 moves that cut nothing run at the machine's `link_feed` (when it is over `max_feed`): links through cleared
/// ground, stay-down links, moves in air. From a simulation of the setup: a move that removes next to nothing
/// and takes no load sample. Plunges and ramps down, probing, and the last millimetre before a cut keep their
/// feeds; cutting moves stay at `max_feed` or under. Moves are split only so a long link's ends can keep the cut
/// feed, and joined again where they end up at one feed.
pub fn link_feeds(cam: &Cam, part: &Part, stock_in: Option<&StockMap>, mut paths: Vec<Toolpath>) -> Vec<Toolpath> {
    let link = cam.machine.link_feed;
    if !(link > cam.machine.max_feed && cam.machine.max_feed > 0.0) {
        return paths;
    }
    let eligible = |tp: &Toolpath| tp.probe.is_none() && cam.op(tp.op).is_some_and(|o| !matches!(o.kind, OpKind::Probe | OpKind::Tap | OpKind::Drill | OpKind::Thread));
    // split long level-ish feed moves into a head, a middle and an approach
    let mut origin: Vec<Vec<usize>> = vec![];
    for tp in paths.iter_mut() {
        if !eligible(tp) {
            origin.push((0..tp.moves.len()).collect());
            continue;
        }
        let (mut out, mut o) = (Vec::with_capacity(tp.moves.len()), Vec::with_capacity(tp.moves.len()));
        let mut p = DVec3::ZERO;
        for (k, m) in tp.moves.iter().enumerate() {
            let q = m.end(p);
            let len = p.distance(q);
            match *m {
                Move::Feed(_, f) if k > 0 && !plunging(p, q) && len > LINK_SPLIT => {
                    for t in [LEAVE / len, 1.0 - APPROACH / len, 1.0] {
                        let at = if t >= 1.0 { q } else { p + (q - p) * t };
                        out.push(Move::Feed(at.to_array(), f));
                        o.push(k);
                    }
                }
                _ => {
                    out.push(*m);
                    o.push(k);
                }
            }
            p = q;
        }
        tp.moves = out;
        origin.push(o);
    }
    let air = |paths: &[Toolpath]| -> Vec<Vec<bool>> {
        let stock = stock_box(cam, part);
        let mut sim = Sim::new(cam, part, stock, paths, crate::cam_paths::sim_resolution(stock));
        if let Some(m) = stock_in {
            sim.start_from(m);
        }
        sim.profile = Some(vec![]);
        sim.removed_by_move = Some(vec![(0.0, 0.0); sim.total()]);
        sim.run_to(usize::MAX, usize::MAX);
        let removed = sim.removed_by_move.take().unwrap_or_default();
        let prof = sim.profile.take().unwrap_or_default();
        let b = bases(paths);
        let mut out: Vec<Vec<bool>> = paths.iter().map(|tp| vec![true; tp.moves.len()]).collect();
        for s in prof.iter().filter(|s| s.area > 1e-3) {
            if let Some((i, k)) = locate(&b, s.step) {
                out[i][k] = false;
            }
        }
        for (i, tp) in paths.iter().enumerate() {
            let mut p = DVec3::ZERO;
            for (k, m) in tp.moves.iter().enumerate() {
                let q = m.end(p);
                let (vol, deep) = removed.get(b[i] + k).copied().unwrap_or((0.0, 0.0));
                // (`deep` is 1 where stock stood within two cells of the tool's side: see `Sim::removed_by_move`)
                if vol as f64 > AIR_AREA * p.distance(q).max(0.2) || deep > 0.05 {
                    out[i][k] = false;
                }
                p = q;
            }
        }
        out
    };
    let a = air(&paths);
    let mut raised: Vec<Vec<bool>> = paths.iter().map(|tp| vec![false; tp.moves.len()]).collect();
    for (i, tp) in paths.iter_mut().enumerate() {
        if !eligible(tp) {
            continue;
        }
        let n = tp.moves.len();
        let mut ends = vec![DVec3::ZERO; n + 1];
        let mut p = DVec3::ZERO;
        for (k, m) in tp.moves.iter().enumerate() {
            ends[k] = p;
            p = m.end(p);
        }
        ends[n] = p;
        for k in 1..n {
            let Move::Feed(to, _) = tp.moves[k] else { continue };
            let (from, q) = (ends[k], ends[k + 1]);
            // plunges and ramps keep their feeds
            if !a[i][k] || q.z < from.z - 1e-6 {
                continue;
            }
            // the approach: the move (a long one's split-off last millimetre) right before a cut, a plunge or a ramp
            // keeps its feed
            let next_cuts = match tp.moves.get(k + 1) {
                Some(Move::Feed(..)) => !a[i][k + 1] || plunging(q, ends[k + 2]) || ends[k + 2].z < q.z - 1e-6,
                Some(Move::Tap { .. }) => true,
                _ => false,
            };
            if next_cuts {
                continue;
            }
            tp.moves[k] = Move::Feed(to, link);
            raised[i][k] = true;
        }
    }
    for (i, tp) in paths.iter_mut().enumerate() {
        tp.moves = join(&tp.moves, &origin[i]);
    }
    // check, on the moves as they will run: a raised move that takes any load after all goes back
    let a2 = air(&paths);
    for (i, tp) in paths.iter_mut().enumerate() {
        let mut any = false;
        for k in 0..tp.moves.len() {
            if let Move::Feed(to, f) = tp.moves[k] {
                if f > cam.machine.max_feed && !a2[i][k] {
                    tp.moves[k] = Move::Feed(to, cam.machine.max_feed);
                }
                any |= f > cam.machine.max_feed;
            }
        }
        if any || raised[i].iter().any(|r| *r) {
            if let Some(tool) = cam.path_tool(tp.op, tp.tool) {
                crate::cam_paths::toolpath_stats(tp, &tool, cam);
            }
        }
    }
    paths
}
