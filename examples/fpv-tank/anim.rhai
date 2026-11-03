// Animation of the FPV tank, called by parts.rhai:
//   rcad script examples/fpv-tank/parts.rhai out animate [max frames]
// Drives the tank around a small arena with skid steering (the hull's speed and turn rate come from
// the two track speeds), turns the wheels, sprockets and track grousers to match, traverses the
// turret, elevates the gun and fires the IR beam at a second tank. Every frame is rendered twice,
// side by side: a chase camera, and the view from the tank's own camera in the gun drum (the barrel
// shows at the bottom of that view as the aim mark). Writes out/frames/f0000.png …; ffmpeg turns
// them into a video.

// ---------------------------------------------------------------- transforms

// rotate a point about the line through (cx, *, cz) along +Y by deg (same as Shape.rotate)
fn rot_y(p, cx, cz, deg) {
    let a = deg.to_radians();
    let x = p.x - cx; let z = p.z - cz;
    vec3(cx + x * cos(a) + z * sin(a), p.y, cz - x * sin(a) + z * cos(a))
}
fn rot_z(p, deg) {
    let a = deg.to_radians();
    vec3(p.x * cos(a) - p.y * sin(a), p.x * sin(a) + p.y * cos(a), p.z)
}

// the tank's frame → the world: yaw about the turret axis (x = y = 0) then move to (px, py)
fn to_world(s, pose) { s.rotate_z(pose.yaw).translate(vec3(pose.x, pose.y, 0)) }
fn pt_world(p, pose) { rot_z(p, pose.yaw) + vec3(pose.x, pose.y, 0) }

// grousers on the outside of a fitted track, shifted along the belt by `phase` (mm)
fn grousers(hp, band, y0, w, phase) {
    let ribs = [];
    let next = 4.0 + ((phase % 8.0) + 8.0) % 8.0 - 8.0; let run = 0.0;
    for i in 0..hp.len() {
        let a = hp[i]; let c = hp[(i + 1) % hp.len()];
        let dx = c.x - a.x; let dz = c.y - a.y;
        let l = (dx * dx + dz * dz).sqrt();
        if l < 1e-9 { continue; }
        let tx = dx / l; let tz = dz / l; let nx = tz; let nz = -tx;
        while next < run { next += 8.0; }
        while next <= run + l {
            let u = next - run;
            let px = a.x + tx * u + nx * (band + 0.1); let pz = a.y + tz * u + nz * (band + 0.1);
            let q = [[px - tx * 1.2 - nx * 0.3, pz - tz * 1.2 - nz * 0.3, 0.0], [px + tx * 1.2 - nx * 0.3, pz + tz * 1.2 - nz * 0.3, 0.0],
                     [px + tx * 1.0 + nx * 1.0, pz + tz * 1.0 + nz * 1.0, 0.0], [px - tx * 1.0 + nx * 1.0, pz - tz * 1.0 + nz * 1.0, 0.0]];
            ribs.push(extrude(polygon(q), vec3(0, 0, w)));
            next += 8.0;
        }
        run += l;
    }
    compound(ribs).rotate_x(90.0).translate(vec3(0, y0 + w, 0))
}

// every part of one tank at a pose: s_l / s_r are how far each track has run (mm), psi the turret
// angle, elev the gun elevation (degrees); hit lights the cupola
fn mir(sh, side) { if side == 0 { sh } else { sh.mirror(vec3(0, 0, 0), vec3(0, 1, 0)) } }

fn tank_parts(t, pose, s_l, s_r, psi, elev, colors, hit, own) {
    let ny = vec3(0, 1, 0);
    let parts = [];
    for p in t.hull { parts.push(#{shape: to_world(p.shape, pose), color: if p.paint { colors.hull } else { p.color }}); }
    for side in [0, 1] {
        let s = if side == 0 { s_l } else { s_r };
        for wl in t.wheels {
            let w = mir(wl.shape, side).rotate(vec3(wl.x, 0, wl.z), ny, (s / wl.r).to_degrees());
            parts.push(#{shape: to_world(w, pose), color: "#454a3e"});
        }
        parts.push(#{shape: to_world(mir(t.band, side), pose), color: "#1e1e1e"});
        parts.push(#{shape: to_world(mir(grousers(t.hull_pts, t.band_t, t.track_y0, t.track_w, -s), side), pose), color: "#1e1e1e"});
    }
    for p in t.turret {
        parts.push(#{shape: to_world(p.shape.rotate_z(psi), pose), color: if p.paint { colors.turret } else { p.color }});
    }
    for p in t.gun {
        let g = p.shape.rotate(vec3(t.gx, 0, t.gz), ny, -elev).rotate_z(psi);
        let part = #{shape: to_world(g, pose), color: if p.paint { colors.turret } else { p.color }};
        if own { part.name = p.name; }
        parts.push(part);
    }
    parts.push(#{shape: to_world(t.cupola.rotate_z(psi), pose), color: if hit { "#ff2a2a" } else { colors.turret }});
    parts
}

// where the tank's camera is and where it looks
fn fpv_camera(t, pose, psi, elev) {
    let eye = vec3(t.gx + 12.0, 0, t.cam_z);
    let ahead = vec3(t.gx + 112.0, 0, t.cam_z);
    let up = vec3(t.gx + 12.0, 0, t.cam_z + 10.0);
    let w = [];
    for p in [eye, ahead, up] { w.push(pt_world(rot_z(rot_y(p, t.gx, t.gz, -elev), psi), pose)); }
    #{eye: w[0], target: w[1], up: w[2] - w[0]}
}

// the muzzle and the beam direction
fn muzzle(t, pose, psi, elev) {
    let m = pt_world(rot_z(rot_y(vec3(t.muzzle_x, 0, t.barrel_z), t.gx, t.gz, -elev), psi), pose);
    let f = pt_world(rot_z(rot_y(vec3(t.muzzle_x + 1.0, 0, t.barrel_z), t.gx, t.gz, -elev), psi), pose);
    #{at: m, dir: f - m}
}

// ---------------------------------------------------------------- the run

fn run(t, out) {
    let FPS = 15.0;
    let B = 2.0 * (t.track_y0 + t.track_w / 2.0);   // track centre distance: 89 mm
    // segments: seconds, left and right track speed (mm/s), turret angle and gun elevation at the end, fire
    let pivot = (PI() / 2.0) * B / (2.0 * 70.0);    // time for a 90° pivot turn at 70 mm/s per track
    let plan = [
        [1.6, 220.0, 220.0, 0.0, 0.0, false],
        [pivot, -70.0, 70.0, 0.0, 0.0, false],
        [1.4, 220.0, 220.0, 0.0, 0.0, false],
        [1.6, 0.0, 0.0, -48.0, 12.0, false],
        [0.6, 0.0, 0.0, -48.0, 1.3, false],
        [0.8, 0.0, 0.0, -48.0, 1.3, true],
        [1.0, 0.0, 0.0, 0.0, 0.0, false],
        [1.8, 240.0, 160.0, 0.0, 0.0, false],
    ];

    // the arena: a checker floor (100 mm tiles), crates, a second tank to shoot at
    let light = []; let dark = [];
    for i in -6..14 { for j in -6..12 {
        let b = block(vec3(i * 100.0, j * 100.0, -2), vec3(i * 100.0 + 100.0, j * 100.0 + 100.0, 0));
        if (i + j) % 2 == 0 { light.push(b); } else { dark.push(b); }
    } }
    let arena = [#{shape: compound(light), color: "#d9d6cc"}, #{shape: compound(dark), color: "#bdb9ad"}];
    for c in [[560, -140, 0], [140, 520, 30], [980, 300, 15], [620, 950, -20]] {
        arena.push(#{shape: block(vec3(-60, -60, 0), vec3(60, 60, 90)).rotate_z(c[2]).translate(vec3(c[0], c[1], 0)), color: "#a07a4a"});
    }
    let enemy_pose = #{x: 740.0, y: 660.0, yaw: -140.0};
    let enemy_colors = #{hull: "#7d6a48", turret: "#8e7a55"};
    let our_colors = #{hull: "#4e5b38", turret: "#5f6e45"};

    let pose = #{x: 0.0, y: 0.0, yaw: 0.0};
    let s_l = 0.0; let s_r = 0.0; let psi = 0.0; let elev = 0.0;
    let cam_target = vec3(0, 0, 40);
    let frame = 0;
    let log = [];
    for seg in plan {
        let n = (seg[0] * FPS).round().to_int();
        let psi0 = psi; let elev0 = elev;
        for k in 1..=n {
            if frame >= t.max_frames { break; }
            let dt = seg[0] / n;
            let f = k / n.to_float();
            let ease = 0.5 - 0.5 * cos(PI() * f);
            // skid steering: forward speed is the mean of the tracks, turn rate their difference over B
            let v = (seg[1] + seg[2]) / 2.0;
            let wz = (seg[2] - seg[1]) / B;
            let a = pose.yaw.to_radians() + wz * dt / 2.0;
            pose.x += v * cos(a) * dt; pose.y += v * sin(a) * dt;
            pose.yaw += (wz * dt).to_degrees();
            s_l += seg[1] * dt; s_r += seg[2] * dt;
            psi = psi0 + (seg[3] - psi0) * ease;
            elev = elev0 + (seg[4] - elev0) * ease;
            let firing = seg[5];

            let ours = tank_parts(t, pose, s_l, s_r, psi, elev, our_colors, false, true);
            let theirs = tank_parts(t, enemy_pose, 0.0, 0.0, 0.0, 0.0, enemy_colors, firing && k > 2, false);
            let scene = arena + ours + theirs;
            if firing {
                // the IR beam (invisible in real life), drawn up to the target
                let m = muzzle(t, pose, psi, elev);
                let len = distance(m.at, vec3(enemy_pose.x, enemy_pose.y, m.at.z));
                scene.push(#{shape: cylinder(m.at, m.dir, 1.6, len), color: "#ff3b30", opacity: 0.55});
            }
            // chase camera: follows the tank smoothly from a fixed direction
            let want = vec3(pose.x, pose.y, 40);
            cam_target = cam_target + (want - cam_target) * 0.15;
            let chase = #{parts: scene, eye: cam_target + vec3(-430, -470, 330), target: cam_target, up: [0, 0, 1], fov: 34};
            // the tank's own camera: everything except the drum and the camera itself
            let fpv_parts = [];
            for p in scene { if !p.contains("name") || (p.name != "gun-drum" && p.name != "pi camera" && p.name != "ir-led") { fpv_parts.push(p); } }
            let c = fpv_camera(t, pose, psi, elev);
            let fpv = #{parts: fpv_parts, eye: c.eye, target: c.target, up: c.up, fov: 41};
            let name = "000" + frame;
            name = name.sub_string(name.len() - 4);
            render_views([chase, fpv], #{path: out + "frames/f" + name + ".png", width: 640, height: 480, columns: 2, background: "#eef0ea"});
            frame += 1;
        }
        log.push(#{x: pose.x, y: pose.y, yaw: pose.yaw, turret: psi, gun: elev});
    }
    #{frames: frame, fps: FPS, track_centres_mm: B, after_each_segment: log}
}
