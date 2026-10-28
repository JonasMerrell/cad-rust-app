// A 5-axis desktop robot arm (base yaw, shoulder, elbow, wrist pitch, wrist roll) with a gear
// gripper, built with the scripting API:
//   cad-rust-app script examples/robot-arm/parts.rhai [output folder]
// Writes one STEP per part (in the part's own frame), placements.json (how each part sits in the
// arm), a render of the arm, mass properties and the joint torques the motors must hold.
// Units mm. World: Z up, the arm reaches along +X, joint axes J2–J4 along Y.
// Machined parts (6061): bottom plate, top plate, turntable, yoke base, yoke cheeks (2), upper arm, forearm.
// Printed parts (PETG, STEP for the slicer): base shroud, wrist link, gripper palm, fingers.
// Bought parts are stand-ins with their outline sizes: NEMA17 steppers with planetary gearboxes
// (J1–J3), MG996R servos (J4, J5), an SG90 (gripper), flange hubs, bearings, standoffs, screws.

let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };

fn zcyl(x, y, z0, r, h) { cylinder(vec3(x, y, z0), vec3(0, 0, 1), r, h) }

// holes on a circle in the XY plane, starting at angle a0 (degrees)
fn ring_holes(n, pcd, a0, z0, r, h) {
    let hs = [];
    for i in 0..n {
        let a = (a0 + 360.0 * i / n).to_radians();
        hs.push(zcyl(pcd / 2.0 * cos(a), pcd / 2.0 * sin(a), z0, r, h));
    }
    hs
}

// two round ends joined by straight sides, lying along X from x0 to x1
fn stadium(x0, x1, w, z0, h) {
    fuse([block(vec3(x0, -w / 2.0, z0), vec3(x1, w / 2.0, z0 + h)), zcyl(x0, 0, z0, w / 2.0, h), zcyl(x1, 0, z0, w / 2.0, h)]).unify()
}

// socket head cap screw: head on z 0..d, shank down to -len
fn shcs(d, len) {
    let head = zcyl(0, 0, 0, 0.8 * d, d).cut([extrude(polygon([[0.43 * d, 0.0, d * 0.4], [0.215 * d, 0.372 * d, d * 0.4], [-0.215 * d, 0.372 * d, d * 0.4],
        [-0.43 * d, 0.0, d * 0.4], [-0.215 * d, -0.372 * d, d * 0.4], [0.215 * d, -0.372 * d, d * 0.4]]), vec3(0, 0, d))]);
    fuse([head, zcyl(0, 0, -len, d / 2.0, len)]).unify()
}

// NEMA17 stepper with a Ø36 planetary gearbox: mounting face z = 0, body below, Ø8 shaft up
fn geared_stepper() {
    let motor = block(vec3(-21.15, -21.15, -70), vec3(21.15, 21.15, -30)).chamfer(3.5);
    let gear = zcyl(0, 0, -30, 18, 30).cut(ring_holes(4, 28, 45, -6, 2, 7));
    fuse([motor, gear, zcyl(0, 0, 0, 11, 2), zcyl(0, 0, 2, 4, 18)]).unify()
}

// flange hub for an Ø8 shaft: flange face z = 0 (Ø32 × 4, 4 × M3 on PCD 24), body up to z = 14
fn flange_hub() {
    fuse([zcyl(0, 0, 0, 16, 4), zcyl(0, 0, 4, 7, 10)]).unify().cut([zcyl(0, 0, -1, 4, 16)] + ring_holes(4, 24, 0, -1, 1.7, 6))
}

// MG996R servo: output axis +Z, spline centre at the origin, top face z = 0, horn on z 4..6.5
fn mg996r() {
    let body = block(vec3(-30.5, -9.85, -37), vec3(10.2, 9.85, 0));
    let tabs = block(vec3(-37.4, -9.0, -9.5), vec3(17.1, 9.0, -7)).cut([zcyl(-34.4, 5, -10, 2.2, 4), zcyl(-34.4, -5, -10, 2.2, 4), zcyl(14.1, 5, -10, 2.2, 4), zcyl(14.1, -5, -10, 2.2, 4)]);
    fuse([body, tabs, zcyl(0, 0, 0, 2.95, 4), zcyl(0, 0, 4, 10, 2.5)]).unify()
}

// SG90 micro servo: output axis +Z, spline at the origin, top face z = 0
fn sg90() {
    let body = block(vec3(-5.9, -6.1, -22.7), vec3(16.6, 6.1, 0));
    let tabs = block(vec3(-10.75, -6.1, -6.5), vec3(21.45, 6.1, -4)).cut([zcyl(-8.45, 0, -7, 1, 4), zcyl(19.15, 0, -7, 1, 4)]);
    fuse([body, tabs, zcyl(0, 0, 0, 2.4, 3.5)]).unify()
}

// spur gear outline, module 1, 16 teeth (pitch Ø16), trapezoid teeth, phase in degrees
fn gear16(cx, cy, z0, h, phase) {
    let pts = [];
    for i in 0..16 {
        let t = phase + 22.5 * i;
        for p in [[t - 7.5, 6.75], [t - 3.5, 9.0], [t + 3.5, 9.0], [t + 7.5, 6.75]] {
            let a = p[0].to_radians();
            pts.push([cx + p[1] * cos(a), cy + p[1] * sin(a), z0]);
        }
    }
    extrude(polygon(pts), vec3(0, 0, h))
}

// a gripper finger: gear at (px, py), arm along +X; side = +1 above the roll axis, -1 below
fn finger(px, py, yc, side, phase, driven) {
    let z0 = 357.5;
    let arm = block(vec3(px, py - 4, z0), vec3(px + 62, py + 4, z0 + 6));
    let pad = if side > 0 { block(vec3(px + 46, yc + 2, z0), vec3(px + 62, py, z0 + 6)) } else { block(vec3(px + 46, py, z0), vec3(px + 62, yc - 2, z0 + 6)) };
    let parts = [gear16(px, py, z0, 6, phase), arm, pad];
    if driven { parts.push(zcyl(px, py, z0 - 4, 3.25, 4)); } else { parts.push(zcyl(px, py, z0 - 0.5, 3.0, 0.5)); }
    let f = fuse(parts).unify();
    let hole = if driven { zcyl(px, py, z0 - 5, 2.4, 12) } else { zcyl(px, py, z0 - 1, 1.6, 8) };
    // grip ridges on the pad face
    let ridges = [];
    for k in 0..4 {
        let x = px + 49 + 3.5 * k;
        let y = if side > 0 { yc + 2 } else { yc - 2 };
        ridges.push(cylinder(vec3(x, y, z0 - 1), vec3(0, 0, 1), 0.6, 8));
    }
    f.cut([hole] + ridges)
}

// ================================================================ machined parts (own frames)

// bottom plate Ø180 × 8: standoff screws counterbored from below, 4 × Ø6.6 table holes
let bottom_plate = zcyl(0, 0, 0, 90, 8).cut(ring_holes(4, 120, 45, -1, 2.75, 10) + ring_holes(4, 120, 45, -1, 4.75, 6.5) + ring_holes(4, 164, 0, -1, 3.3, 10));

// top plate Ø150 × 12: J1 gearbox pilot and screws, thrust bearing seat, standoff screws
let top_plate = zcyl(0, 0, 0, 75, 12).cut([zcyl(0, 0, -1, 11.1, 14), zcyl(0, 0, 7, 30.5, 6)] + ring_holes(4, 28, 45, -1, 2.25, 14) + ring_holes(4, 120, 45, -1, 2.75, 14));

// turntable Ø140 × 12: J1 hub (M3 tapped), yoke screws (M5 tapped)
let tt_holes = [zcyl(0, 0, -1, 5, 14)] + ring_holes(4, 24, 0, -1, 1.25, 14);
for p in [[42, 18], [42, -18], [-42, 18], [-42, -18]] { tt_holes.push(zcyl(p[0], p[1], -1, 2.1, 14)); }
let turntable = zcyl(0, 0, 0, 70, 12).cut(tt_holes);

// shoulder yoke, in three 2.5D pieces (a one-piece yoke needs a 64 mm deep slot): a base 100 × 62 × 12
// with 1 mm relief pockets the cheeks sit in, and two cheeks 10 thick bolted on from below (M4 into
// their bottom edges). J2 axis 45 above the base's underside; the cheeks are 42 apart.
let ZA = 45.0;
let base_cuts = [block(vec3(-34, 20.95, 11), vec3(34, 32, 13)), block(vec3(-34, -32, 11), vec3(34, -20.95, 13))];
for p in [[42, 18], [42, -18], [-42, 18], [-42, -18]] {
    base_cuts.push(zcyl(p[0], p[1], -1, 2.75, 14));
    base_cuts.push(zcyl(p[0], p[1], 12 - 5.5, 4.75, 6));
}
for p in [[18, 26], [-18, 26], [18, -26], [-18, -26]] {
    base_cuts.push(zcyl(p[0], p[1], -1, 2.25, 14));
    base_cuts.push(zcyl(p[0], p[1], -1, 4.0, 5.5));
}
let yoke_base = block(vec3(-50, -31, 0), vec3(50, 31, 12)).cut(base_cuts);
// cheek, lying flat: bottom edge on y = 0, J2 axis at (0, CH), 10 thick in z
let CH = ZA - 11.0;
let cheek_cuts = [zcyl(0, CH, -1, 11.1, 12), cylinder(vec3(18, -1, 5), vec3(0, 1, 0), 1.65, 13), cylinder(vec3(-18, -1, 5), vec3(0, 1, 0), 1.65, 13)]
    + ring_holes(4, 28, 45, -1, 2.25, 12).map(|h| h.translate(vec3(0, CH, 0)));
let cheek = fuse([block(vec3(-30, 0, 0), vec3(30, CH, 10)), zcyl(0, CH, 0, 30, 10)]).unify().cut(cheek_cuts);
// the three pieces in the yoke's frame (for the FEA)
let yoke = [yoke_base, cheek.rotate(vec3(0, 0, 0), vec3(1, 0, 0), 90.0).translate(vec3(0, 31, 11)), cheek.rotate(vec3(0, 0, 0), vec3(1, 0, 0), 90.0).translate(vec3(0, -21, 11))];

// upper arm: 200 between joints, 50 wide, 12 thick, lying flat (shoulder at the origin)
let L1 = 200.0;
let ua_cuts = [zcyl(0, 0, -1, 5, 14), zcyl(L1, 0, -1, 11.1, 14), zcyl(L1, 0, 7, 18.5, 6),
    extrude(rounded_rect(vec3(70, 0, 5), 60, 28, 6), vec3(0, 0, 8)), extrude(rounded_rect(vec3(140, 0, 5), 60, 28, 6), vec3(0, 0, 8))]
    + ring_holes(8, 24, 0, -1, 1.25, 14) + ring_holes(4, 28, 45, -1, 2.25, 14).map(|h| h.translate(vec3(200, 0, 0)));
let upper_arm = stadium(0, L1, 50, 0, 12).cut(ua_cuts);

// forearm: 170 from the elbow to the wrist pitch axis, MG996R window at the wrist end
let L2 = 170.0;
let fa_cuts = [zcyl(0, 0, -1, 5, 14), extrude(rounded_rect(vec3(77.5, 0, 5), 85, 28, 6), vec3(0, 0, 8)),
    extrude(rounded_rect(vec3(L2 - 10.15, 0, -1), 41.9, 20.5, 2), vec3(0, 0, 14))] + ring_holes(4, 24, 0, -1, 1.25, 14);
for p in [[L2 - 34.4, 5], [L2 - 34.4, -5], [L2 + 14.1, 5], [L2 + 14.1, -5]] { fa_cuts.push(zcyl(p[0], p[1], -1, 1.65, 14)); }
let forearm = stadium(0, L2, 50, 0, 12).cut(fa_cuts);

// ================================================================ printed parts (built in place)

// base shroud between the plates, with a cable notch at the back
let shroud = zcyl(0, 0, 8, 75, 75).cut([zcyl(0, 0, 7, 72.5, 77), block(vec3(-80, -12, 7), vec3(-60, 12, 30))]);

// wrist link: horn plate on the J4 servo horn + a sleeve holding the J5 servo along X
let yc = -65.85;  // J5 roll axis: y = yc, z = 360
let horn_holes = [cylinder(vec3(170, -45, 360), vec3(0, -1, 0), 3.5, 20)];
for p in [[177, 360], [163, 360], [170, 367], [170, 353]] { horn_holes.push(cylinder(vec3(p[0], -45, p[1]), vec3(0, -1, 0), 1.1, 20)); }
let plate = block(vec3(152, -55, 338), vec3(200.5, -49, 402)).fillet(2);
let sleeve = block(vec3(172, -79.2, 340), vec3(200.5, -54, 400)).cut([block(vec3(170, -76.2, 349.3), vec3(202, -55.5, 391.2))]);
let tab_holes = [];
for p in [[yc + 5, 394.4], [yc - 5, 394.4], [yc + 5, 345.9], [yc - 5, 345.9]] { tab_holes.push(cylinder(vec3(205, p[0], p[1]), vec3(-1, 0, 0), 1.1, 12)); }
let wrist_link = fuse([plate, sleeve]).unify().cut(horn_holes + tab_holes);

// gripper palm: plate on the J5 horn + a deck carrying the SG90 and the finger pivots
let pa_holes = [cylinder(vec3(214, yc, 360), vec3(1, 0, 0), 3.5, 12)];
for p in [[yc + 7, 360], [yc - 7, 360], [yc, 367], [yc, 353]] { pa_holes.push(cylinder(vec3(214, p[0], p[1]), vec3(1, 0, 0), 1.1, 12)); }
let sx = 238.0;  // finger pivots at (sx, yc ± 8)
pa_holes.push(zcyl(sx, yc + 8, 350, 3.5, 10));
pa_holes.push(zcyl(sx, yc - 8, 350, 1.6, 10));
pa_holes.push(zcyl(sx - 8.45, yc + 8, 340, 1.0, 14));
pa_holes.push(zcyl(sx + 19.15, yc + 8, 340, 1.0, 14));
let palm = fuse([block(vec3(216.5, yc - 22, 344), vec3(222.5, yc + 22, 376)), block(vec3(222.5, yc - 22, 353), vec3(262, yc + 22, 357)),
    block(vec3(sx - 12.5, yc + 4, 349), vec3(sx - 4.5, yc + 12, 353)), block(vec3(sx + 15.5, yc + 4, 349), vec3(sx + 23.5, yc + 12, 353))]).unify().cut(pa_holes);
let finger_a = finger(sx, yc + 8, yc, 1, 0.0, true);
let finger_b = finger(sx, yc - 8, yc, -1, 11.25, false);

// ================================================================ bought parts
let motor = geared_stepper();
let hub = flange_hub();
let servo = mg996r();
let micro = sg90();
let standoff = zcyl(0, 0, 0, 5, 75).cut([zcyl(0, 0, -1, 2.1, 13), zcyl(0, 0, 63, 2.1, 13)]);
let thrust = fuse([zcyl(0, 0, 0, 30, 4).cut([zcyl(0, 0, -1, 20.2, 6)]), zcyl(0, 0, 9, 30, 4).cut([zcyl(0, 0, 8, 20, 6)]), zcyl(0, 0, 4, 27, 5).cut([zcyl(0, 0, 3, 23, 7)])]);
let b608 = zcyl(0, 0, 0, 11, 7).cut([zcyl(0, 0, -1, 4, 9)]);
let stub = zcyl(0, 0, 0, 4, 20);
let m3x8 = shcs(3, 8);  let m4x12 = shcs(4, 12);  let m4x16 = shcs(4, 16);  let m5x12 = shcs(5, 12);  let m5x16 = shcs(5, 16);  let m3x12 = shcs(3, 12);

// ================================================================ placements
// Each instance: part, rotation (axis, angle) about the part origin, then position. Matches
// assembly_insert's {position, rotate}.
let RX90 = [[1, 0, 0], 90.0];  let RXM90 = [[1, 0, 0], -90.0];  let RX180 = [[1, 0, 0], 180.0];  let ID = [[0, 0, 1], 0.0];
let ZS = 160.0;  // J2 axis height
let ZE = ZS + L1; // J3
let inst = [];
fn put(group, part, rot, pos) { this.push(#{group: group, part: part, axis: rot[0], angle: rot[1], pos: pos}); }

inst.put("base", "Bottom plate", ID, [0, 0, 0]);
inst.put("base", "Shroud", ID, [0, 0, 0]);
inst.put("base", "Top plate", ID, [0, 0, 83]);
inst.put("base", "Thrust bearing 51108", ID, [0, 0, 90]);
inst.put("J1", "Turntable", ID, [0, 0, 103]);
inst.put("J1", "Yoke base", ID, [0, 0, 115]);
inst.put("J1", "Yoke cheek", RX90, [0, 31, 126]);
inst.put("J1", "Yoke cheek", RX90, [0, -21, 126]);
for p in [[18, 26], [-18, 26], [18, -26], [-18, -26]] { inst.put("J1", "SHCS M4x16", RX180, [p[0], p[1], 119.5]); }
inst.put("base", "Stepper J1", ID, [0, 0, 83]);
inst.put("J1", "Flange hub", RX180, [0, 0, 103]);
for i in 0..4 {
    let a = (45.0 + 90.0 * i).to_radians();
    inst.put("base", "Standoff M5 x 75", ID, [60 * cos(a), 60 * sin(a), 8]);
    inst.put("base", "SHCS M5x12", RX180, [60 * cos(a), 60 * sin(a), 5.5]);
    inst.put("base", "SHCS M5x12", ID, [60 * cos(a), 60 * sin(a), 95]);
    inst.put("base", "SHCS M4x12", ID, [14 * cos(a), 14 * sin(a), 90]);
    let b = (90.0 * i).to_radians();
    inst.put("J1", "SHCS M3x8", RX180, [12 * cos(b), 12 * sin(b), 99]);
}
for p in [[42, 18], [42, -18], [-42, 18], [-42, -18]] { inst.put("J1", "SHCS M5x16", ID, [p[0], p[1], 121.5]); }

// J2 shoulder
inst.put("J1", "Stepper J2", RX90, [0, 31, ZS]);
inst.put("J2", "Upper arm", [[1, 1, 1], -120.0], [0, -6, ZS]);
inst.put("J2", "Flange hub", RXM90, [0, 6, ZS]);
inst.put("J2", "Flange hub", RX90, [0, -6, ZS]);
inst.put("J2", "Stub shaft 8 x 20", RX90, [0, -10, ZS]);
inst.put("J1", "Bearing 608", RX90, [0, -21, ZS]);
for i in 0..4 {
    let a = (45.0 + 90.0 * i).to_radians();
    inst.put("J1", "SHCS M4x16", RX90, [14 * cos(a), 21, ZS + 14 * sin(a)]);
    inst.put("J2", "SHCS M4x12", RX90, [14 * cos(a), -6, ZE + 14 * sin(a)]);
    let b = (90.0 * i).to_radians();
    inst.put("J2", "SHCS M3x8", RXM90, [12 * cos(b), 10, ZS + 12 * sin(b)]);
    inst.put("J2", "SHCS M3x8", RX90, [12 * cos(b + 0.7853981633974483), -10, ZS + 12 * sin(b + 0.7853981633974483)]);
    inst.put("J3", "SHCS M3x8", RXM90, [12 * cos(b), -17, ZE + 12 * sin(b)]);
}
// J3 elbow
inst.put("J2", "Stepper J3", RX90, [0, 1, ZE]);
inst.put("J3", "Forearm", RXM90, [0, -33, ZE]);
inst.put("J3", "Flange hub", RXM90, [0, -21, ZE]);
// J4 wrist pitch, J5 wrist roll, gripper
inst.put("J3", "Servo MG996R J4", RX90, [L2, -42.5, ZE]);
for p in [[L2 - 34.4, 5], [L2 - 34.4, -5], [L2 + 14.1, 5], [L2 + 14.1, -5]] { inst.put("J3", "SHCS M4x12", RX90, [p[0], -35.5, ZE + p[1]]); }
inst.put("J4", "Wrist link", ID, [0, 0, 0]);
inst.put("J4", "Servo MG996R J5", [[0, 1, 0], 90.0], [210, yc, ZE]);
inst.put("J5", "Gripper palm", ID, [0, 0, 0]);
inst.put("J5", "Servo SG90 gripper", ID, [sx, yc + 8, 353]);
inst.put("FA", "Finger (driven)", ID, [0, 0, 0]);
inst.put("FB", "Finger (idler)", ID, [0, 0, 0]);
inst.put("J5", "SHCS M3x12", ID, [sx, yc - 8, 363.5]);

let parts = #{
    "Bottom plate": #{shape: bottom_plate, kind: "machined", material: "Aluminium 6061", color: "#b8c2cc"},
    "Top plate": #{shape: top_plate, kind: "machined", material: "Aluminium 6061", color: "#b8c2cc"},
    "Turntable": #{shape: turntable, kind: "machined", material: "Aluminium 6061", color: "#c9d2db"},
    "Yoke base": #{shape: yoke_base, kind: "machined", material: "Aluminium 6061", color: "#c9d2db"},
    "Yoke cheek": #{shape: cheek, kind: "machined", material: "Aluminium 6061", color: "#c9d2db"},
    "Upper arm": #{shape: upper_arm, kind: "machined", material: "Aluminium 6061", color: "#d0d8e0"},
    "Forearm": #{shape: forearm, kind: "machined", material: "Aluminium 6061", color: "#d0d8e0"},
    "Shroud": #{shape: shroud, kind: "printed", material: "PETG", color: "#e8742a"},
    "Wrist link": #{shape: wrist_link, kind: "printed", material: "PETG", color: "#e8742a"},
    "Gripper palm": #{shape: palm, kind: "printed", material: "PETG", color: "#e8742a"},
    "Finger (driven)": #{shape: finger_a, kind: "printed", material: "PETG", color: "#3a3f45"},
    "Finger (idler)": #{shape: finger_b, kind: "printed", material: "PETG", color: "#3a3f45"},
    "Stepper J1": #{shape: motor, kind: "bought", material: "", color: "#2b2f33", mass: 550.0},
    "Stepper J2": #{shape: motor, kind: "bought", material: "", color: "#2b2f33", mass: 550.0},
    "Stepper J3": #{shape: motor, kind: "bought", material: "", color: "#2b2f33", mass: 550.0},
    "Servo MG996R J4": #{shape: servo, kind: "bought", material: "", color: "#1d3d7a", mass: 55.0},
    "Servo MG996R J5": #{shape: servo, kind: "bought", material: "", color: "#1d3d7a", mass: 55.0},
    "Servo SG90 gripper": #{shape: micro, kind: "bought", material: "", color: "#2a64c8", mass: 9.0},
    "Flange hub": #{shape: hub, kind: "bought", material: "Aluminium 6061", color: "#d6a83a"},
    "Standoff M5 x 75": #{shape: standoff, kind: "bought", material: "Aluminium 6061", color: "#9aa3ab"},
    "Thrust bearing 51108": #{shape: thrust, kind: "bought", material: "Steel", color: "#7d858c"},
    "Bearing 608": #{shape: b608, kind: "bought", material: "Steel", color: "#7d858c"},
    "Stub shaft 8 x 20": #{shape: stub, kind: "bought", material: "Steel", color: "#7d858c"},
    "SHCS M3x8": #{shape: m3x8, kind: "bought", material: "Steel", color: "#202326"},
    "SHCS M3x12": #{shape: m3x12, kind: "bought", material: "Steel", color: "#202326"},
    "SHCS M4x12": #{shape: m4x12, kind: "bought", material: "Steel", color: "#202326"},
    "SHCS M4x16": #{shape: m4x16, kind: "bought", material: "Steel", color: "#202326"},
    "SHCS M5x12": #{shape: m5x12, kind: "bought", material: "Steel", color: "#202326"},
    "SHCS M5x16": #{shape: m5x16, kind: "bought", material: "Steel", color: "#202326"},
};

let density = #{"Aluminium 6061": 2.70e-3, "Steel": 7.85e-3, "PETG": 1.27e-3 * 0.6};  // printed at ~60 % fill

// ================================================================ place, check, weigh
let placed = [];
let shapes = [];
let bom = #{};
let mass_upper = [];   // moves with J2 (and J3 sits on it)
let mass_fore = [];    // moves with J3
let mass_wrist = [];   // hangs on the forearm's servo screws (J4 servo and everything after it)
let total_mass = 0.0;
for it in inst {
    let p = parts[it.part];
    let s = p.shape.rotate(vec3(0, 0, 0), it.axis, it.angle).translate(it.pos);
    shapes.push(s);
    placed.push(#{shape: s, color: p.color});
    bom[it.part] = if bom.contains(it.part) { bom[it.part] + 1 } else { 1 };
    let m = if p.contains("mass") { point_mass(p.mass, s.centroid()) } else { mass_props(s, density[p.material]) };
    total_mass += m.mass;
    if ["J4", "J5", "FA", "FB"].contains(it.group) || it.part == "Servo MG996R J4" { mass_wrist.push(m); }
    if ["J3", "J4", "J5", "FA", "FB"].contains(it.group) { mass_fore.push(m); } else if it.group == "J2" { mass_upper.push(m); }
}

// payload 500 g between the finger pads
let payload = point_mass(500.0, vec3(sx + 54, yc, 360.5));
mass_fore.push(payload);
mass_wrist.push(payload);
let wrist = combine(mass_wrist);

// holding torques (N·m) with the arm stretched out horizontally (worst case for J2, J3)
let g = 9.81e-3;  // N per gram; masses in g and levers in mm give N·mm, / 1000 for N·m
let fore = combine(mass_fore);
let upper = combine(mass_upper);
let t_elbow = fore.mass * g * (fore.cg.x - 0.0) / 1000.0;
let t_shoulder = (upper.mass * g * (upper.cg.z - ZS) + fore.mass * g * (L1 + fore.cg.x)) / 1000.0;

// interference: pairs of neighbouring structural parts that must not overlap
let checks = [];
for pair in [["Upper arm", "Yoke cheek"], ["Yoke cheek", "Yoke base"], ["Forearm", "Upper arm"], ["Servo MG996R J4", "Forearm"], ["Wrist link", "Servo MG996R J4"],
             ["Gripper palm", "Servo MG996R J5"], ["Finger (driven)", "Finger (idler)"], ["Stepper J3", "Upper arm"], ["Stepper J2", "Yoke cheek"]] {
    let a = ();  let b = ();
    for i in 0..inst.len() {
        if inst[i].part == pair[0] && type_of(a) == "()" { a = shapes[i]; }
        if inst[i].part == pair[1] && type_of(b) == "()" { b = shapes[i]; }
    }
    let v = (a & b).volume();
    checks.push(`${pair[0]} / ${pair[1]}: ${v.to_string()} mm³`);
}

// ================================================================ outputs
let summary = #{parts: #{}, instances: [], bom: bom};
for name in parts.keys() {
    let p = parts[name];
    let file = name;
    file.replace(" ", "-");  file.replace("(", "");  file.replace(")", "");
    file += ".step";
    export_step(p.shape, out + file);
    let mm = if p.contains("mass") { p.mass } else { mass_props(p.shape, density[p.material]).mass };
    summary.parts[name] = #{file: file, kind: p.kind, material: p.material, color: p.color, mass_g: mm, bbox: p.shape.bbox().size, valid: p.shape.is_valid()};
}
for it in inst { summary.instances.push(#{group: it.group, part: it.part, axis: it.axis, angle: it.angle, pos: it.pos}); }
summary.total_mass_g = total_mass;
summary.payload_g = 500.0;
summary.forearm_group = #{mass_g: fore.mass, cg: fore.cg};
summary.upper_group = #{mass_g: upper.mass, cg: upper.cg};
summary.wrist_group = #{mass_g: wrist.mass, cg: wrist.cg};
summary.holding_torque_Nm = #{shoulder_J2: t_shoulder, elbow_J3: t_elbow};
summary.interference = checks;
write_json(out + "placements.json", summary);
export_step(shapes, out + "robot-arm-assembly.step");
export_step(yoke, out + "Shoulder-yoke-assembly.step");
render(placed, #{path: out + "robot-arm.png", width: 1400, height: 1100, view: "iso"});
render(placed, #{path: out + "robot-arm-front.png", width: 1100, height: 1100, view: "front"});
#{total_mass_g: total_mass, torque: summary.holding_torque_Nm, fore: fore.mass, upper: upper.mass, interference: checks}
