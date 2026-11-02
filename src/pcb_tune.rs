//! Lengths: routed net lengths, length and matched-length rules, and length tuning with
//! accordion meanders (Altium's interactive length tuning, done for a whole net at once).

use crate::pcb::{self, Cu, Issue, Netlist, Pcb, Track};
use crate::pcb_lib::P;

/// Routed length of a net: its tracks, plus a via barrel (the board thickness) per via.
pub fn net_length(p: &Pcb, net: &str) -> f64 {
    let t: f64 = p.board.tracks.iter().filter(|t| t.net == net).map(|t| pcb::dist(t.a, t.b)).sum();
    let v = p.board.vias.iter().filter(|v| v.net == net).count() as f64 * p.board.thickness;
    t + v
}

/// The nets a length rule covers (names in the netlist matching its patterns).
pub fn rule_nets(nl: &Netlist, r: &pcb::LengthRule) -> Vec<String> {
    nl.nets.iter().filter(|n| r.nets.iter().any(|pat| pcb::net_matches(pat, &n.name))).map(|n| n.name.clone()).collect()
}

fn net_at(p: &Pcb, net: &str) -> P {
    p.board.tracks.iter().find(|t| t.net == net).map(|t| t.a).unwrap_or(p.board.outline.first().copied().unwrap_or([0.0; 2]))
}

/// Length, matched-length and differential pair issues. Nets with no tracks yet are left to the
/// unrouted check.
pub fn length_issues(p: &Pcb, nl: &Netlist) -> Vec<Issue> {
    let mut out = vec![];
    let routed = |n: &str| p.board.tracks.iter().any(|t| t.net == n);
    for r in &p.board.rules.lengths {
        let nets: Vec<String> = rule_nets(nl, r).into_iter().filter(|n| routed(n)).collect();
        let lens: Vec<f64> = nets.iter().map(|n| net_length(p, n)).collect();
        for (n, l) in nets.iter().zip(&lens) {
            if r.min > 0.0 && *l < r.min - 1e-6 {
                out.push(Issue::new("length", net_at(p, n), true, format!("{n} is {l:.2} mm long; rule {} wants at least {} mm", r.name, r.min)));
            }
            if r.max > 0.0 && *l > r.max + 1e-6 {
                out.push(Issue::new("length", net_at(p, n), true, format!("{n} is {l:.2} mm long; rule {} allows {} mm", r.name, r.max)));
            }
        }
        if r.tolerance > 0.0 && nets.len() > 1 {
            let top = lens.iter().cloned().fold(0.0, f64::max);
            for (n, l) in nets.iter().zip(&lens) {
                if top - l > r.tolerance + 1e-6 {
                    out.push(Issue::new("matched", net_at(p, n), true, format!("{n} is {:.2} mm shorter than the longest net of {} (tolerance {} mm)", top - l, r.name, r.tolerance)));
                }
            }
        }
    }
    for d in &p.board.rules.pairs {
        if !routed(&d.p) || !routed(&d.n) {
            continue;
        }
        let (a, b) = (net_length(p, &d.p), net_length(p, &d.n));
        if (a - b).abs() > 0.5 {
            out.push(Issue::new("pair", net_at(p, &d.p), false, format!("Pair {}: {} is {a:.2} mm and {} is {b:.2} mm (more than 0.5 mm apart; tune the shorter one)", d.name, d.p, d.n)));
        }
    }
    out
}

/// The polyline of one meander run replacing the straight track a → b: `n` trombone loops of
/// height `amp` and pitch `pitch` on side `side` (+1 left of a → b), centred on the track.
fn meander(a: P, b: P, n: usize, amp: f64, pitch: f64, side: f64) -> Vec<P> {
    let l = pcb::dist(a, b);
    let d = [(b[0] - a[0]) / l, (b[1] - a[1]) / l];
    let nrm = [-d[1] * side, d[0] * side];
    let span = 2.0 * pitch * n as f64;
    let start = (l - span) / 2.0;
    let at = |s: f64, h: f64| [a[0] + d[0] * s + nrm[0] * h, a[1] + d[1] * s + nrm[1] * h];
    let mut v = vec![a];
    for k in 0..n {
        let s0 = start + 2.0 * pitch * k as f64;
        v.push(at(s0, 0.0));
        v.push(at(s0, amp));
        v.push(at(s0 + pitch, amp));
        v.push(at(s0 + pitch, 0.0));
    }
    v.push(at(start + span, 0.0));
    v.push(b);
    v.dedup_by(|x, y| pcb::dist(*x, *y) < 1e-9);
    v
}

/// Whether new copper of `net` is clear of other nets, holes, keepouts and the board edge.
fn clear(p: &Pcb, nl: &Netlist, pads: &[pcb::WPad], segs: &[(P, P)], w: f64, layer: pcb::Layer, net: &str) -> bool {
    let its = pcb::items(p, nl, pads, &[]);
    let r = &p.board.rules;
    let c = p.net_rules(nl, net).clearance;
    for (a, b) in segs {
        let cu = Cu::Seg { a: *a, b: *b, r: w / 2.0 };
        for it in &its {
            if it.net == net || it.layer.is_some_and(|l| l != layer) {
                continue;
            }
            let need = c.max(p.net_rules(nl, &it.net).clearance);
            if cu.gap(&it.cu) < need - 1e-4 {
                return false;
            }
        }
        for h in &p.board.holes {
            if cu.gap(&Cu::Seg { a: h.at, b: h.at, r: h.d / 2.0 }) < c {
                return false;
            }
        }
        for k in p.board.keepouts.iter().filter(|k| k.tracks && k.on(Some(layer)) && k.outline.len() >= 3) {
            if cu.gap(&Cu::Poly(k.outline.clone())) <= 0.0 {
                return false;
            }
        }
        if p.board.outline.len() >= 3 {
            if !pcb::inside(&p.board.outline, *a) || !pcb::inside(&p.board.outline, *b) {
                return false;
            }
            let near = pcb::outline_dist(&p.board.outline, *a).min(pcb::outline_dist(&p.board.outline, *b));
            if near < r.edge + w / 2.0 {
                return false;
            }
        }
    }
    true
}

/// Lengthen a net to `target` mm with meanders on its longest straight tracks, where they fit
/// clear of other copper. Returns the length reached (a net longer than the target is left as
/// it is).
pub fn tune(p: &mut Pcb, net: &str, target: f64) -> Result<f64, String> {
    let nl = p.netlist();
    if nl.by_name(net).is_none() {
        return Err(format!("no net {net}"));
    }
    if !p.board.tracks.iter().any(|t| t.net == net) {
        return Err(format!("{net} has no tracks to tune"));
    }
    let pads = p.pads(&nl);
    let c = p.net_rules(&nl, net).clearance;
    for _ in 0..64 {
        let need = target - net_length(p, net);
        if need < 0.01 {
            break;
        }
        // the longest straight tracks first
        let mut order: Vec<usize> = (0..p.board.tracks.len()).filter(|&i| p.board.tracks[i].net == net).collect();
        order.sort_by(|a, b| {
            let l = |i: usize| pcb::dist(p.board.tracks[i].a, p.board.tracks[i].b);
            l(*b).total_cmp(&l(*a))
        });
        let mut done = false;
        for ti in order {
            let t = p.board.tracks[ti].clone();
            let w = t.width;
            let l = pcb::dist(t.a, t.b);
            let pitch = (2.0 * w + c).max(3.0 * w);
            // leave a pitch at each end so the meander starts clear of the corner or pad
            let room = l - 2.0 * pitch;
            let most = (room / (2.0 * pitch)).floor() as usize;
            if most == 0 {
                continue;
            }
            // about 1 mm loops first (even, compact accordions), taller or smaller ones where those collide
            let mut placed = false;
            for amp_max in [1.0f64, 1.5, 0.6, 2.5, 4.0] {
                let amp_max: f64 = amp_max.max(2.0 * w);
                let n = ((need / (2.0 * amp_max)).ceil() as usize).clamp(1, most);
                let amp = (need / (2.0 * n as f64)).min(amp_max);
                if amp < 0.01 {
                    continue;
                }
                for side in [1.0, -1.0] {
                    let pts = meander(t.a, t.b, n, amp, pitch, side);
                    let segs: Vec<(P, P)> = pts.windows(2).map(|s| (s[0], s[1])).collect();
                    // check against the board without this track
                    let mut trial = p.clone();
                    trial.board.tracks.remove(ti);
                    if clear(&trial, &nl, &pads, &segs, w, t.layer, net) {
                        p.board.tracks.remove(ti);
                        for (a, b) in segs {
                            p.board.tracks.push(Track { a, b, width: w, layer: t.layer, net: net.into() });
                        }
                        placed = true;
                        break;
                    }
                }
                if placed {
                    break;
                }
            }
            if placed {
                done = true;
                break;
            }
        }
        if !done {
            break;
        }
    }
    Ok(net_length(p, net))
}

/// Tune every net of a matched-length rule up to its longest net. Returns (net, before, after).
pub fn match_lengths(p: &mut Pcb, rule: &pcb::LengthRule) -> Vec<(String, f64, f64)> {
    let nl = p.netlist();
    let nets: Vec<String> = rule_nets(&nl, rule).into_iter().filter(|n| p.board.tracks.iter().any(|t| &t.net == n)).collect();
    let mut target = nets.iter().map(|n| net_length(p, n)).fold(0.0, f64::max);
    if rule.min > 0.0 {
        target = target.max(rule.min);
    }
    let mut out = vec![];
    for n in nets {
        let before = net_length(p, &n);
        let after = if target - before > rule.tolerance.max(0.01) / 2.0 { tune(p, &n, target).unwrap_or(before) } else { before };
        out.push((n, before, after));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcb::{Layer, LengthRule};

    #[test]
    fn meanders_reach_the_target_and_stay_clear() {
        let mut p = crate::pcb::tests::led_board();
        p.board.outline = crate::pcb::rect_outline(60.0, 50.0, 0.0);
        let nl = p.netlist();
        let pads = p.pads(&nl);
        // a long straight VCC track across open board
        let a = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap().at;
        p.board.tracks.push(Track { a, b: [a[0], 40.0], width: 0.25, layer: Layer::Top, net: "VCC".into() });
        let before = net_length(&p, "VCC");
        let got = tune(&mut p, "VCC", before + 12.0).unwrap();
        assert!((got - (before + 12.0)).abs() < 0.05, "{before} → {got}");
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let issues = pcb::drc(&p, &nl, &pads, &[]);
        assert!(!issues.iter().any(|i| i.rule == "clearance" || i.rule == "short" || i.rule == "edge"), "{issues:?}");
        // a length rule flags a net that is too short, and a matched group gets evened up
        p.board.rules.lengths.push(LengthRule { name: "L".into(), nets: vec!["VCC".into()], min: got + 5.0, ..Default::default() });
        let nl = p.netlist();
        assert!(length_issues(&p, &nl).iter().any(|i| i.rule == "length"));
        p.board.rules.lengths[0] = LengthRule { name: "M".into(), nets: vec!["VCC".into(), "GND".into()], tolerance: 0.2, ..Default::default() };
        let j2 = pads.iter().find(|x| x.part == 2 && x.num == "2").unwrap().at;
        p.board.tracks.push(Track { a: j2, b: [j2[0], j2[1] - 15.0], width: 0.25, layer: Layer::Bottom, net: "GND".into() });
        assert!(length_issues(&p, &nl).iter().any(|i| i.rule == "matched"));
        let rule = p.board.rules.lengths[0].clone();
        let res = match_lengths(&mut p, &rule);
        assert_eq!(res.len(), 2);
        assert!(length_issues(&p, &nl).iter().all(|i| i.rule != "matched"), "{res:?}");
    }
}
