//! Timing: Klipper's look-ahead planner (trapezoids, junction deviation from
//! `square_corner_velocity`, the Z speed and accel limits), filling each move's start and end time.

use super::cfg::Machine;
use super::interp::{Kind, Move};

struct Plan {
    d: f64,
    accel: f64,
    max_cruise_v2: f64,
    max_start_v2: f64,
    delta_v2: f64,
    unit: [f64; 3],
    kinematic: bool,
    start_v2: f64,
    end_v2: f64,
}

/// Fill `t0`/`t1` of every move; returns the total time (s).
pub fn plan(m: &Machine, moves: &mut [Move]) -> f64 {
    let jd = m.scv * m.scv * (std::f64::consts::SQRT_2 - 1.0) / m.max_accel.max(1.0);
    let mut ps: Vec<Plan> = Vec::with_capacity(moves.len());
    for mv in moves.iter() {
        let d3 = [mv.to[0] - mv.from[0], mv.to[1] - mv.from[1], mv.to[2] - mv.from[2]];
        let d = (d3[0] * d3[0] + d3[1] * d3[1] + d3[2] * d3[2]).sqrt();
        let kinematic = d > 1e-9 && mv.kind != Kind::Dwell;
        let (mut v, mut a) = (mv.speed.min(m.max_velocity), m.max_accel);
        let (dd, unit) = if kinematic {
            if d3[2].abs() > 1e-9 {
                let r = d / d3[2].abs();
                v = v.min(m.max_z_velocity * r);
                a = a.min(m.max_z_accel * r);
            }
            (d, [d3[0] / d, d3[1] / d, d3[2] / d])
        } else if mv.kind == Kind::Dwell {
            (0.0, [0.0; 3])
        } else {
            v = mv.speed.min(m.max_extrude_only_velocity.max(1.0));
            (mv.e.abs(), [0.0; 3])
        };
        let mut p = Plan { d: dd, accel: a, max_cruise_v2: v * v, max_start_v2: 0.0, delta_v2: 2.0 * dd * a, unit, kinematic, start_v2: 0.0, end_v2: 0.0 };
        if let Some(prev) = ps.last() {
            if p.kinematic && prev.kinematic {
                let cos = -(prev.unit[0] * p.unit[0] + prev.unit[1] * p.unit[1] + prev.unit[2] * p.unit[2]);
                if cos <= 0.999999 {
                    let cos = cos.max(-0.999999);
                    let s = (0.5 * (1.0 - cos)).sqrt();
                    let r_jd = s / (1.0 - s);
                    let tan = s / (0.5 * (1.0 + cos)).sqrt();
                    let cv = 0.5 * p.d * tan * p.accel;
                    let pcv = 0.5 * prev.d * tan * prev.accel;
                    p.max_start_v2 =
                        (r_jd * jd * p.accel).min(r_jd * jd * prev.accel).min(cv).min(pcv).min(p.max_cruise_v2).min(prev.max_cruise_v2).min(prev.max_start_v2 + prev.delta_v2);
                }
            }
        }
        ps.push(p);
    }
    // backward pass: how fast each move may end so the machine can still stop
    let mut next_start = 0.0f64;
    for p in ps.iter_mut().rev() {
        p.end_v2 = next_start;
        p.start_v2 = p.max_start_v2.min(p.end_v2 + p.delta_v2);
        next_start = p.start_v2;
    }
    // forward pass: how fast each one can have got by its end
    let mut prev_end = 0.0f64;
    for p in ps.iter_mut() {
        p.start_v2 = p.start_v2.min(prev_end);
        p.end_v2 = p.end_v2.min(p.start_v2 + p.delta_v2);
        prev_end = p.end_v2;
    }
    let mut t = 0.0;
    for (mv, p) in moves.iter_mut().zip(&ps) {
        mv.t0 = t;
        let dt = if mv.kind == Kind::Dwell {
            mv.speed
        } else if p.d <= 1e-12 {
            0.0
        } else {
            let (vs, ve) = (p.start_v2.sqrt(), p.end_v2.sqrt());
            let cruise_v2 = p.max_cruise_v2.min((p.start_v2 + p.end_v2 + 2.0 * p.d * p.accel) / 2.0);
            let vc = cruise_v2.sqrt();
            let da = (cruise_v2 - p.start_v2) / (2.0 * p.accel);
            let dd = (cruise_v2 - p.end_v2) / (2.0 * p.accel);
            let dc = (p.d - da - dd).max(0.0);
            (vc - vs) / p.accel + (vc - ve) / p.accel + if vc > 0.0 { dc / vc } else { 0.0 }
        };
        t += dt.max(0.0);
        mv.t1 = t;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gsim::interp::Interp;

    #[test]
    fn trapezoid() {
        let m = Machine { max_velocity: 100.0, max_accel: 1000.0, ..Machine::default() };
        let mut it = Interp::new(&m);
        it.assume_homed([0.0; 3]);
        it.run_file("G1 X100 F60000\n");
        let t = plan(&m, &mut it.moves);
        // accelerate 0.1 s over 5 mm, cruise 90 mm at 100 mm/s, decelerate 0.1 s
        assert!((t - 1.1).abs() < 1e-9, "{t}");
    }
}
