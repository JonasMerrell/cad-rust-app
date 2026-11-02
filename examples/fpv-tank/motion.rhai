// Motion check of the FPV tank, called by parts.rhai:
//   rcad script examples/fpv-tank/parts.rhai out motion
// Sweeps the turret through ±90° and the gun through its elevation range together and reports every
// overlap between moving and fixed parts; checks the elevation linkage, the track and sprocket
// geometry and the drive and servo loads. Prints a JSON report.

fn overlap(a, b) {
    let p = a.bbox(); let q = b.bbox();
    if p.max.x < q.min.x || q.max.x < p.min.x || p.max.y < q.min.y || q.max.y < p.min.y || p.max.z < q.min.z || q.max.z < p.min.z { return 0.0; }
    (a & b).volume()
}

fn run(t) {
    let ny = vec3(0, 1, 0);
    let report = #{};

    // ---- turret × gun sweep
    let clashes = [];
    let poses = 0;
    for psi in [-90.0, -60.0, -30.0, 0.0, 30.0, 60.0, 90.0] {
        for e in [t.elev[0], 0.0, 10.0, t.elev[1]] {
            let moving = [];
            for p in t.turret { moving.push(#{name: p.name, shape: p.shape.rotate_z(psi)}); }
            // the parallelogram linkage turns the horn by the same angle as the gun
            moving.push(#{name: "elevation horn", shape: t.horn.rotate(vec3(t.servo_x, 0, t.gz), ny, -e).rotate_z(psi)});
            for p in t.gun { moving.push(#{name: p.name, shape: p.shape.rotate(vec3(t.gx, 0, t.gz), ny, -e).rotate_z(psi)}); }
            // the pushrod stays parallel to itself and moves with the arm tips
            let a = e.to_radians();
            let rod = t.rod.translate(vec3(t.arm * sin(a), 0, t.arm * (1.0 - cos(a))));
            moving.push(#{name: "pushrod", shape: rod.rotate_z(psi)});
            for m in moving {
                for f in t.fixed {
                    let v = overlap(m.shape, f.shape);
                    if v > 0.5 { clashes.push(#{turret: psi, gun: e, moving: m.name, fixed: f.name, mm3: v}); }
                }
            }
            // the gun against the turret it sits in
            for g in t.gun + [#{name: "pushrod", shape: t.rod, rod: true}] {
                let gs = if g.contains("rod") { rod } else { g.shape.rotate(vec3(t.gx, 0, t.gz), ny, -e) };
                let others = t.turret + [#{name: "elevation horn", shape: t.horn.rotate(vec3(t.servo_x, 0, t.gz), ny, -e)}];
                if g.contains("rod") { for q in t.gun { others.push(#{name: q.name, shape: q.shape.rotate(vec3(t.gx, 0, t.gz), ny, -e)}); } }
                for p in others {
                    let v = overlap(gs, p.shape);
                    if v > 0.5 { clashes.push(#{turret: psi, gun: e, moving: g.name, fixed: p.name, mm3: v}); }
                }
            }
            poses += 1;
        }
    }
    report.sweep = #{poses: poses, turret: [-90, 90], gun: t.elev, clashes: clashes};

    // ---- elevation linkage: horn and drum arm, both r long and pointing down at level, rod between
    let lk = [];
    let r = t.arm; let H = [t.servo_x, t.gz]; let G = [t.gx, t.gz];
    let rod = G[0] - H[0];
    for e in [t.elev[0], 0.0, 10.0, t.elev[1]] {
        let a = e.to_radians();
        // drum arm tip (nose up moves the tip forward), horn tip for the same servo angle
        let d = [G[0] + r * sin(a), G[1] - r * cos(a)];
        let h = [H[0] + r * sin(a), H[1] - r * cos(a)];
        let len = ((d[0] - h[0]) ** 2 + (d[1] - h[1]) ** 2).sqrt();
        // transmission angle: between the rod and the horn arm (90° is ideal)
        let ux = d[0] - h[0]; let uz = d[1] - h[1];
        let vx = h[0] - H[0]; let vz = h[1] - H[1];
        let tr = (((ux * vx + uz * vz) / (len * r)).abs()).acos().to_degrees();
        lk.push(#{gun: e, servo: e, rod_length: len, transmission_deg: tr});
    }
    report.linkage = #{arm_mm: r, rod_mm: rod, poses: lk, note: "parallelogram: rod length stays constant, so servo angle = gun angle"};

    // ---- drivetrain geometry
    let perim = 0.0;
    let hp = t.hull_pts;
    for i in 0..hp.len() { let a = hp[i]; let b = hp[(i + 1) % hp.len()]; perim += ((b.x - a.x) ** 2 + (b.y - a.y) ** 2).sqrt(); }
    report.track = #{
        // the pitch line is the band's mid-plane: 0.1 clearance + half the band outside the wheels
        belt_pitch_line_mm: perim + 2.0 * PI() * (0.1 + t.band / 2.0), printed_pitch_line_mm: t.lugs * t.pitch,
        stretch_pct: ((perim + 2.0 * PI() * (0.1 + t.band / 2.0)) / (t.lugs * t.pitch) - 1.0) * 100.0,
        sprocket_pitch_on_pitch_line_mm: 2.0 * PI() * (t.spr_r + 0.1 + t.band / 2.0) / t.spr_n, lug_pitch_mm: t.pitch,
        lug_width_mm: 6.0, groove_width_mm: 7.0, lug_height_mm: 2.5, groove_depth_mm: 3.0,
        lug_length_mm: 4.0, sprocket_pocket_mm: 5.2, pocket_depth_mm: 2.9,
    };

    // ---- loads (stated assumptions)
    let g = 9.81;
    let m = t.mass_g / 1000.0;
    let W = m * g;
    let L = t.contact_mm / 1000.0;                 // track contact length
    let B = t.track_centres_mm / 1000.0;
    let mu_turn = 0.8;                             // TPU sliding sideways on a hard floor (pessimistic)
    let mu_roll = 0.08;                            // rolling resistance of a small rubber-band track
    let rs = (t.spr_r + 0.1 + t.band / 2.0) / 1000.0;    // effective sprocket radius to the band's middle
    // pivot turn: resisting moment μ W L / 4 against the two tracks pulling opposite ways a distance B apart
    let F_pivot = mu_turn * W * L / (4.0 * B);
    let F_roll = mu_roll * W / 2.0;
    let F_acc = m * 1.0 / 2.0;                     // 1 m/s² of acceleration, per track
    let T_need = (F_pivot + F_roll) * rs;
    let T_n20_rated = 0.25 * g / 100.0;            // N20 300 rpm: about 0.25 kg·cm rated
    let T_n20_stall = 1.5 * g / 100.0;             //                about 1.5 kg·cm stall
    report.drive = #{
        assumptions: "330 g, 80 mm track contact, 89 mm track centres, μ 0.8 sideways, 0.08 rolling, N20 300 rpm (0.25 kg·cm rated, 1.5 kg·cm stall)",
        pivot_turn_force_per_track_N: F_pivot, rolling_force_per_track_N: F_roll, accel_force_per_track_N: F_acc,
        torque_needed_pivot_Nm: T_need, torque_needed_straight_Nm: (F_roll + F_acc) * rs,
        n20_rated_Nm: T_n20_rated, n20_stall_Nm: T_n20_stall,
        margin_pivot_vs_rated: T_n20_rated / T_need,
        top_speed_m_s: 300.0 / 60.0 * 2.0 * PI() * rs,
    };
    // servos: the gun is tiny; the turret only has to overcome friction of the ring on the deck
    let gun_mass = t.gun_mass_g / 1000.0;
    let T_gun = gun_mass * g * t.gun_cg_offset_mm / 1000.0;
    let T_turret = 0.3 * t.turret_mass_g / 1000.0 * g * 0.028;   // μ 0.3 PLA on PLA at the 28 mm bearing radius
    let T_sg90 = 1.8 * g / 100.0;
    report.servos = #{gun_torque_Nm: T_gun, turret_torque_Nm: T_turret, sg90_stall_Nm: T_sg90,
                      margin_gun: T_sg90 / T_gun, margin_turret: T_sg90 / T_turret};
    report
}
