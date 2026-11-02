// Prototype of the robot arm's L50 cycloidal gearbox (50:1, Ø120) as a standalone bench test unit:
// the gearbox with its NEMA 17 bolted on directly, an L-shaped stand that clamps to the bench, an
// output flange and a 250 mm test lever with weight holes at 50/100/150/200 mm, plus a small
// tolerance coupon to tune the printer before the big parts.
//   rcad script examples/cycloidal-gearbox-prototype/parts.rhai [output folder] [quick]
// Writes a STEP and an STL per printed part, the assembly as STEP, renders and summary.json
// (BOM, masses, meshing and interference checks at several input angles).
//
// Same gearbox as J2 in examples/robot-arm-6dof (L50 spec), with these changes so it can be built:
//  - the cam is two parts (A and B). As one part the bearing for the first lobe can't get past the
//    second lobe. Both halves have a D-bore, so the motor shaft's flat sets them 180° apart.
//  - a 2 mm ledge under the output bearing. Before, the bearing's outer race sat over the ring-pin
//    bore (Ø96) with nothing under it. The ledge also caps the ring pins.
//  - the carrier has a lip under the bearing's inner race and the output flange is a separate
//    part bolted on top, so the carrier is held both ways along the axis.
//  - counterbored motor screws, since the first disc runs 0.5 mm above the housing floor.
//  - the disc bore is R + 0.3 (the pins sit 157° deep in their grooves). At R the lobe tips, which
//    reach 48.02 between the pins, rubbed the bore.
//  - disc outlines sampled every 0.25 mm along the curve (see cyc_pts); the arm's even sampling
//    leaves chords that cut 0.11 mm into the pins after the 0.08 clearance.
//  - standard dowel lengths: ring pins 3x20, output pins 6x30.
// The arm's J2 drives this gearbox through a 2:1 belt and an Ø8 shaft. The prototype drives it
// straight from the motor shaft, which tests the same cycloid stage.
// Units mm. Gearbox local frame: output axis +Z, housing rear face z = 0, motor below. In the
// assembly the axis is horizontal (output towards -Y), H above the bench.

let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };
let FILES = !(ARGS.len() > 1 && ARGS[1] == "quick");

fn zcyl(x, y, z0, r, h) { cylinder(vec3(x, y, z0), vec3(0, 0, 1), r, h) }
fn ring_holes(n, pcd, a0, z0, r, h) {
    let hs = [];
    for i in 0..n {
        let a = (a0 + 360.0 * i / n).to_radians();
        hs.push(zcyl(pcd / 2.0 * cos(a), pcd / 2.0 * sin(a), z0, r, h));
    }
    hs
}
fn ring(z0, ri, ro, h) { zcyl(0, 0, z0, ro, h).cut([zcyl(0, 0, z0 - 1, ri, h + 2)]) }
fn soft(s, r) {
    try { return s.fillet(r); } catch (e) { }
    try { return s.fillet(r * 0.5); } catch (e) { }
    s
}
fn circle_pts(cx, cy, r, n) {
    let pts = [];
    for i in 0..n { let a = 6.283185307179586 * i / n; pts.push([cx + r * cos(a), cy + r * sin(a)]); }
    pts
}
fn hull_xy(pts, z, h) {
    let hl = convex_hull(pts);
    extrude(face(wire(hl.map(|p| [p.x, p.y, z]), true)), vec3(0, 0, h))
}
// cycloid disc outline for N ring pins of radius Rr on radius R, eccentricity E (N - 1 lobes),
// resampled to points ds apart along the curve. Even steps in t leave chords up to 1.5 mm long in
// the troughs round each pin, which bulge 0.19 mm into the pins; at ds = 0.25 the bulge is ~0.005.
fn cyc_pts(R, Rr, E, N, ds) {
    let dense = [];
    let steps = (N - 1) * 400;
    for i in 0..steps {
        let t = 6.283185307179586 * i / steps;
        let psi = atan(sin((1.0 - N) * t), R / (E * N) - cos((1.0 - N) * t));
        dense.push([R * cos(t) - Rr * cos(t + psi) - E * cos(N * t), -R * sin(t) + Rr * sin(t + psi) + E * sin(N * t)]);
    }
    let pts = [[dense[0][0], dense[0][1], 0.0]];
    let acc = 0.0;
    for i in 1..steps {
        acc += sqrt((dense[i][0] - dense[i - 1][0]) ** 2 + (dense[i][1] - dense[i - 1][1]) ** 2);
        if acc >= ds { pts.push([dense[i][0], dense[i][1], 0.0]); acc = 0.0; }
    }
    pts
}
// NEMA 17 stepper: mounting face z = 0, body below, Ø5 shaft up
fn nema17(len) {
    let body = block(vec3(-21.15, -21.15, -len), vec3(21.15, 21.15, 0)).chamfer(3.5);
    let shaft = zcyl(0, 0, 0, 2.5, 22).cut([block(vec3(2.0, -3, 2), vec3(3, 3, 23))]);   // D-flat 0.5 deep
    fuse([body, zcyl(0, 0, 0, 11, 2), shaft]).unify()
}
// D-bore for a Ø5 motor shaft with a 0.5 mm flat on +X (bore 5.1, flat at x = 2.05)
fn d_bore(z0, h) {
    zcyl(0, 0, z0, 2.55, h).cut([block(vec3(2.05, -3, z0 - 1.0), vec3(3, 3, z0 + h + 1.0))])
}

// ================================================================ the L50 spec (as in the arm)
let s = #{od: 120.0, R: 48.0, Rr: 1.5, E: 0.8, N: 51, d: 8.0, tr: 6.0, bw: 10.0, bid: 70.0, bod: 90.0,
          ebid: 25.0, ebod: 37.0, ebw: 7.0, rh: 31.0, rp: 3.0, nout: 6, flange_od: 74.0, pcd_mount: 104.0};
let N = s.N;  let E = s.E;  let rr = s.od / 2.0;
let RATIO = N - 1;
let MOTOR_LEN = 60.0;
// axial stack (z)
let z_d1 = s.tr + 0.5;              // disc A
let z_d2 = z_d1 + s.d + 1.0;        // disc B
let z_ledge = z_d2 + s.d + 1.0;     // ledge under the output bearing, caps the ring pins
let LEDGE = 2.0;
let z_b0 = z_ledge + LEDGE;         // output bearing
let z_top = z_b0 + s.bw;            // housing top = carrier top
let z_out = z_top + 6.0;            // output flange face
let z_cam = z_d1 + s.d + 0.5;       // cam A / cam B split (between the discs)
let PIN_Z0 = s.tr - 2.0;            // ring pin bottom (2 mm into the floor), 20 long
let cover_pcd = s.bod / 2.0 + rr;   // Ø105

// ================================================================ gearbox parts
// housing
let hc = [zcyl(0, 0, s.tr, s.R + 0.3, z_ledge - s.tr),               // disc bore: the lobe tips reach R + 0.02 between the pins
          zcyl(0, 0, z_ledge - 0.5, s.bod / 2.0 - 1.5, LEDGE + 1.0),  // ledge inner edge Ø87
          zcyl(0, 0, z_b0, s.bod / 2.0, s.bw + 1.0),                  // output bearing seat Ø90
          zcyl(0, 0, -1, 11.25, s.tr + 2.0)];                         // motor pilot Ø22.5
for p in [[15.5, 15.5], [-15.5, 15.5], [15.5, -15.5], [-15.5, -15.5]] {
    hc.push(zcyl(p[0], p[1], -1, 1.7, s.tr + 2.0));                     // M3 motor screws, from inside
    hc.push(zcyl(p[0], p[1], s.tr - 3.5, 3.25, 4.0));                   // head counterbore Ø6.5 x 3.5
}
for i in 0..N { let a = 6.283185307179586 * i / N; hc.push(zcyl(s.R * cos(a), s.R * sin(a), PIN_Z0, s.Rr, z_ledge - PIN_Z0)); }
hc += ring_holes(6, s.pcd_mount, 0, -1, 2.0, 7) + ring_holes(6, cover_pcd, 30, z_top - 7.0, 2.0, 8);   // M3 heat-set inserts
// two small witness notches on the rim at 0°, to line up with the flange mark when counting turns
hc.push(block(vec3(rr - 1.0, -0.6, z_top - 4.0), vec3(rr + 1.0, 0.6, z_top + 1.0)));
let housing = soft(zcyl(0, 0, 0, rr, z_top), 2.0).cut(hc);

// discs: exact cycloid offset 0.08 mm inward; B is A turned half a lobe
let prof = offset2d(face(wire(cyc_pts(s.R, s.Rr, E, N, 0.25), true)), -0.08)[0];
let blank = extrude(prof, vec3(0, 0, s.d));
let dholes = [zcyl(0, 0, -1, s.ebod / 2.0, s.d + 2.0)] + ring_holes(s.nout, 2.0 * s.rh, 0, -1, s.rp + E, s.d + 2.0);
// B differs from A only by the lobe phase against the holes, and flipping A gives A again, so they
// are marked: one Ø2 dimple on A's top face, two on B's (between the output holes, at 30°)
fn dimples(n, d) { let ds = []; for i in 0..n { let a = (30.0 + 8.0 * i).to_radians(); ds.push(zcyl(25.0 * cos(a), 25.0 * sin(a), d - 0.8, 1.0, 1.0)); } ds }
let discA0 = blank.cut(dholes + dimples(1, s.d));                                       // centred at the origin, z 0..d
let discB0 = blank.rotate(vec3(0, 0, 0), vec3(0, 0, 1), 180.0 / RATIO).cut(dholes + dimples(2, s.d));

// cam A: collar + lobe +E; cam B: lobe -E. Both D-bored.
let camA = fuse([zcyl(0, 0, 2.5, 8, z_d1 - 2.5), zcyl(E, 0, z_d1, s.ebid / 2.0, z_cam - z_d1)]).unify().cut([d_bore(0, 30)]);
let camB = zcyl(-E, 0, z_cam, s.ebid / 2.0, z_d2 + s.d - z_cam).cut([d_bore(0, 30)]);

// carrier: lip under the bearing inner race, plate in the bearing, output pins pressed in from below,
// 6 M3 heat-set inserts in the top face for the flange
let carrier = fuse([zcyl(0, 0, z_ledge, s.bid / 2.0 + 2.5, LEDGE), zcyl(0, 0, z_b0, s.bid / 2.0, s.bw)]).unify()
    .cut([zcyl(0, 0, z_ledge - 1.0, 4, 20)] + ring_holes(s.nout, 2.0 * s.rh, 0, z_ledge - 1.0, s.rp, z_top - z_ledge + 2.0)
         + ring_holes(6, s.flange_od - 12.0, 30, z_top - 7.0, 2.0, 8));
// output flange: clamps the bearing inner race against the carrier lip and spaces the load past the cover
let flange = soft(zcyl(0, 0, z_top, s.flange_od / 2.0, 6), 0.8)
    .cut([zcyl(0, 0, z_top - 1.0, 8, 8)] + ring_holes(6, s.flange_od - 12.0, 30, z_top - 1.0, 1.7, 8)
         + [block(vec3(s.flange_od / 2.0 - 1.5, -0.6, z_out - 1.0), vec3(s.flange_od / 2.0 + 1.0, 0.6, z_out + 1.0))]);
// cover ring over the bearing outer race
let cover = soft(ring(z_top, s.flange_od / 2.0 + 1.0, rr, 3), 1.0).cut(ring_holes(6, cover_pcd, 30, z_top - 1.0, 1.7, 5));

let pin_len = 20.0;  let opin_len = 30.0;
let ring_pins = [];
for i in 0..N { let a = 6.283185307179586 * i / N; ring_pins.push(zcyl(s.R * cos(a), s.R * sin(a), PIN_Z0, s.Rr, pin_len)); }
let opin_z0 = z_top - opin_len;    // flush with the carrier top
let out_pins0 = ring_holes(s.nout, 2.0 * s.rh, 0, opin_z0, s.rp, opin_len);
let bearing_out = ring(z_b0, s.bid / 2.0, s.bod / 2.0, s.bw);
let eb0 = ring(0, s.ebid / 2.0, s.ebod / 2.0, s.ebw);
let motor = nema17(MOTOR_LEN);

// the moving parts at input angle th (degrees): discs orbit at E and turn -th/50, output turns -th/50
fn moving(th, discA0, discB0, eb0, out_pins0, carrier, s, z_d1, z_d2) {
    let E = s.E;  let a = th.to_radians();  let rot = -th / (s.N - 1);
    let c = vec3(E * cos(a), E * sin(a), 0);
    let eo = (s.d - s.ebw) / 2.0;
    #{discA: discA0.rotate(vec3(0, 0, 0), vec3(0, 0, 1), rot).translate(c + vec3(0, 0, z_d1)),
      discB: discB0.rotate(vec3(0, 0, 0), vec3(0, 0, 1), rot).translate(vec3(0, 0, z_d2) - c),
      ebA: eb0.translate(c + vec3(0, 0, z_d1 + eo)), ebB: eb0.translate(vec3(0, 0, z_d2 + eo) - c),
      opins: compound(out_pins0).rotate(vec3(0, 0, 0), vec3(0, 0, 1), rot),
      carrier: carrier.rotate(vec3(0, 0, 0), vec3(0, 0, 1), rot)}
}
fn cam_at(cam, th) { cam.rotate(vec3(0, 0, 0), vec3(0, 0, 1), th) }

// ================================================================ test stand, lever, coupon (gearbox frame; +Y is up)
let H = 100.0;           // axis height above the bench
let PL = 12.0;           // stand plate thickness, behind the housing rear face
let FT = 10.0;           // foot thickness
let FZ0 = -110.0;  let FZ1 = 30.0;   // foot extent along the axis
let FW = 80.0;           // foot half width
// upright: circle around the housing hulled down to the foot
let upright = hull_xy(circle_pts(0, 0, 62, 64) + [[-FW, -H + FT - 1.0], [FW, -H + FT - 1.0]], -PL, PL);
let foot = block(vec3(-FW, -H, FZ0), vec3(FW, -H + FT, FZ1));
let gussets = [];
for x in [-52.0, 44.0] {
    gussets.push(extrude(polygon([[x, -H + FT - 1.0, -PL + 0.5], [x, -H + FT - 1.0, FZ0 + 10.0], [x, -10.0, -PL + 0.5]]), vec3(8, 0, 0)));
}
// motor hole: round with a 45° point on top so it prints standing up without support
let mh = 28.5;
let motor_hole = hull_xy(circle_pts(0, 0, mh, 64) + [[0, mh * 1.4142]], -PL - 1.0, PL + 2.0);
let st_cuts = [motor_hole] + ring_holes(6, s.pcd_mount, 0, -PL - 1.0, 1.7, PL + 2.0) + ring_holes(6, s.pcd_mount, 0, -PL - 1.0, 3.25, 5.0);
for x in [-62.0, 62.0] { for z in [FZ0 + 15.0, FZ1 - 12.0] { st_cuts.push(cylinder(vec3(x, -H - 1.0, z), vec3(0, 1, 0), 3.0, FT + 2.0)); } }
st_cuts.push(block(vec3(-12, -H - 1.0, -95), vec3(12, -H + FT + 1.0, -40)));   // cable slot under the motor
let stand = fuse([soft(upright, 2.0), soft(foot, 2.0)] + gussets).unify().cut(st_cuts);

// test lever: hub on the flange, arm along +X, Ø6.5 weight holes at 50/100/150/200 mm, pointer tip
let LT = 10.0;  let LZ = z_out;
let arm2d = convex_hull(circle_pts(0, 0, s.flange_od / 2.0, 64) + circle_pts(205, 0, 10, 32));
let lever = soft(extrude(face(wire(arm2d.map(|p| [p.x, p.y, LZ]), true)), vec3(0, 0, LT)), 1.5);
let lv_cuts = [zcyl(0, 0, LZ - 1.0, 8, LT + 2.0)] + ring_holes(6, s.flange_od - 12.0, 30, LZ - 1.0, 1.7, LT + 2.0)
    + ring_holes(6, s.flange_od - 12.0, 30, LZ + LT - 5.0, 3.25, 6.0);
for x in [50.0, 100.0, 150.0, 200.0] { lv_cuts.push(zcyl(x, 0, LZ - 1.0, 3.25, LT + 2.0)); }
// lightening slots between the holes
for x in [75.0, 125.0, 175.0] { lv_cuts.push(hull_xy(circle_pts(x - 12.0, 0, 4, 16) + circle_pts(x + 12.0, 0, 4, 16), LZ - 1.0, LT + 2.0)); }
lever = lever.cut(lv_cuts);
// tip pointer for the backlash reading
let pointer = extrude(polygon([[213.0, -4.0, LZ], [226.0, 0.0, LZ], [213.0, 4.0, LZ]]), vec3(0, 0, 3));
lever = fuse([lever, pointer]).unify();

// tolerance coupon, printed flat: every fit the gearbox depends on, at the gearbox's nominal sizes
//  Ø37 bore (6805 outer race, press)    Ø25 boss (6805 inner race, slide)   Ø5 D-bore (motor shaft)
//  Ø3 hole (ring pin, slide)            Ø6 hole (output pin, press)         Ø4 hole (M3 heat-set insert)
//  and a Ø90 bore / Ø70 boss ring pair for the 6814 output bearing
let coupon = soft(block(vec3(0, 0, 0), vec3(96, 46, 6)), 1.0).cut([zcyl(23, 23, -1, s.ebod / 2.0, 10), d_bore(-1, 10).translate(vec3(60, 12, 0)),
    zcyl(60, 32, -1, s.Rr, 10), zcyl(73, 32, -1, s.rp, 10), zcyl(87, 32, -1, 2.0, 10)]);
let boss25 = zcyl(80, 12, 6, s.ebid / 2.0, 7).cut([zcyl(80, 12, 5, 9, 9)]);
coupon = fuse([coupon, boss25]).unify();
let coupon_big = fuse([ring(0, s.bod / 2.0, s.bod / 2.0 + 3.0, 6), zcyl(0, 0, 0, s.bod / 2.0 + 3.0, 1.6),
                       ring(0, s.bid / 2.0 - 2.4, s.bid / 2.0, 8)]).unify()
    .cut([zcyl(0, 0, -1, s.bid / 2.0 - 2.4, 10)]);

// ================================================================ assembly at input angle 0
let m0 = moving(0.0, discA0, discB0, eb0, out_pins0, carrier, s, z_d1, z_d2);
let C_P = "#41464d";  let C_H = "#565d66";  let C_A = "#f07a1c";  let C_D = "#2f3338";  let C_M = "#24272b";  let C_ST = "#8a9299";
let parts = [];
fn add(name, shape, kind, qty, material, color, note) { this.push(#{name: name, shape: shape, kind: kind, qty: qty, material: material, color: color, note: note}); }
parts.add("Housing", housing, "printed", 1, "PA-CF", C_H, "print on its rear face (motor side down)");
parts.add("Cycloid disc A", m0.discA, "printed", 1, "PA-CF", C_D, "flat");
parts.add("Cycloid disc B", m0.discB, "printed", 1, "PA-CF", "#6b7178", "flat");
parts.add("Cam A", camA, "printed", 1, "PA-CF", "#555b62", "collar down, 100% infill");
parts.add("Cam B", camB, "printed", 1, "PA-CF", "#555b62", "flat, 100% infill");
parts.add("Carrier", m0.carrier, "printed", 1, "PA-CF", "#b0b8c0", "lip down");
parts.add("Output flange", flange, "printed", 1, "PA-CF", C_A, "flat");
parts.add("Cover", cover, "printed", 1, "ASA", C_A, "flat");
parts.add("Test stand", stand, "printed", 1, "PA-CF", C_P, "foot down");
parts.add("Test lever", lever, "printed", 1, "PA-CF", C_A, "flat, pointer up");
parts.add("Tolerance coupon", coupon.translate(vec3(150, 150, -H)), "printed", 1, "PA-CF", "#d6a83a", "flat, print first");
parts.add("Tolerance coupon 6814", coupon_big.translate(vec3(150, 260, -H)), "printed", 1, "PA-CF", "#d6a83a", "flat, print first");
parts.add("Ring pins 3x20", compound(ring_pins), "bought", 51, "Steel", C_ST, "ISO 8734 dowel 3 m6 x 20");
parts.add("Output pins 6x30", m0.opins, "bought", 6, "Steel", "#d6a83a", "ISO 8734 dowel 6 m6 x 30");
parts.add("Output bearing 6814", bearing_out, "bought", 1, "Steel", C_ST, "6814-2RS 70x90x10");
parts.add("Cam bearings 6805", compound([m0.ebA, m0.ebB]), "bought", 2, "Steel", C_ST, "6805-2RS 25x37x7");
parts.add("Motor 17HS24", motor, "bought", 1, "motor", C_M, "NEMA 17, 60 mm, ~0.65 N·m, Ø5 D-shaft >= 20 mm");

// ================================================================ checks
let checks = [];
fn find(parts, name) { for p in parts { if p.name == name { return p.shape; } } () }
// static fits
for pair in [["Housing", "Ring pins 3x20"], ["Housing", "Output bearing 6814"], ["Housing", "Motor 17HS24"], ["Housing", "Cycloid disc A"], ["Housing", "Cycloid disc B"],
             ["Housing", "Carrier"], ["Housing", "Cam A"], ["Carrier", "Output bearing 6814"], ["Carrier", "Ring pins 3x20"], ["Carrier", "Cam B"], ["Carrier", "Cycloid disc B"],
             ["Output flange", "Cover"], ["Output flange", "Output bearing 6814"], ["Cover", "Output bearing 6814"], ["Cover", "Housing"], ["Test lever", "Cover"], ["Test lever", "Housing"],
             ["Test stand", "Motor 17HS24"], ["Test stand", "Housing"], ["Cam A", "Motor 17HS24"], ["Cam B", "Motor 17HS24"], ["Cam A", "Cam B"],
             ["Cam A", "Cycloid disc A"], ["Cam B", "Cycloid disc B"], ["Cycloid disc A", "Cycloid disc B"], ["Ring pins 3x20", "Output bearing 6814"]] {
    checks.push(#{pair: pair[0] + " / " + pair[1], overlap_mm3: (find(parts, pair[0]) & find(parts, pair[1])).volume()});
}
// meshing through the cycle: discs vs ring pins and output pins, cams vs discs, at several input angles
let rp = compound(ring_pins);
let mesh = [];
for th in [0.0, 3.6, 17.0, 45.0, 90.0, 137.0, 180.0, 263.0] {
    let m = moving(th, discA0, discB0, eb0, out_pins0, carrier, s, z_d1, z_d2);
    let cA = cam_at(camA, th);  let cB = cam_at(camB, th);
    mesh.push(#{input_deg: th, output_deg: -th / RATIO,
        discA_ringpins: (m.discA & rp).volume(), discB_ringpins: (m.discB & rp).volume(),
        discA_outpins: (m.discA & m.opins).volume(), discB_outpins: (m.discB & m.opins).volume(),
        discA_housing: (m.discA & housing).volume(), discB_carrier: (m.discB & m.carrier).volume(),
        camA_bearingA: (cA & m.ebA).volume(), camB_bearingB: (cB & m.ebB).volume(),
        camA_bearingB: (cA & m.ebB).volume(), camB_discA: (cB & m.discA).volume()});
}
// clearance between disc A and the ring pins / output pins at 0°: grow the pins until they touch
fn first_touch(a, pins, steps) {
    for d in steps { if (a & pins).volume() > 0.0 { return d; } }
    ()
}

// ================================================================ world placement and outputs
// gearbox frame -> world: axis horizontal along -Y (output towards the viewer in the front render), H above the bench
fn world(sh, H) { sh.rotate(vec3(0, 0, 0), vec3(1, 0, 0), 90.0).translate(vec3(0, 0, H)) }
let placed = [];  let shapes = [];
for p in parts {
    if p.name.starts_with("Tolerance") { continue; }
    let w = world(p.shape, H);
    shapes.push(w);
    placed.push(#{shape: w, color: p.color});
}
let bom = [];
let PA_CF = 1.2e-3;   // g/mm³
let total_print_g = 0.0;
for p in parts {
    let file = p.name;
    for c in [" ", "(", ")", "/"] { file.replace(c, "-"); }
    if FILES && p.kind == "printed" {
        let sh = p.shape;
        if p.name.starts_with("Tolerance") { sh = sh.translate(vec3(-150, if p.name.ends_with("6814") { -260 } else { -150 }, H)); }
        if p.name == "Test stand" { sh = world(sh, H); }   // foot down on the bed
        export_step(sh, out + "printed-" + file + ".step");
        export_stl(sh, out + "print-" + file + ".stl", 0.03);
    }
    let g = if p.kind == "printed" { p.shape.volume() * PA_CF } else { () };
    if p.kind == "printed" { total_print_g += g; }
    bom.push(#{part: p.name, kind: p.kind, qty: p.qty, material: p.material, note: p.note, mass_g: g, bbox: p.shape.bbox().size, valid: p.shape.is_valid()});
}
export_step(shapes, out + "cycloidal-gearbox-prototype.step");

// lever self-weight torque about the axis when horizontal (to subtract in the torque test)
let lm = mass_props(lever, PA_CF);
let lever_self_Nm = lm.mass / 1000.0 * 9.81 * lm.cg.x / 1000.0;

render(placed, #{path: out + "gearbox-prototype.png", width: 1600, height: 1200, eye: [400, -470, 330], target: [70, -20, 80], up: [0, 0, 1], fov: 28, background: "#f3f4f6"});
render(placed, #{path: out + "gearbox-prototype-back.png", width: 1600, height: 1200, eye: [-400, 470, 300], target: [10, -20, 80], up: [0, 0, 1], fov: 28, background: "#f3f4f6"});
// exploded along the axis, in assembly order (lever left out)
let ex = [[motor, C_M, -110.0], [housing, C_H, 0.0], [compound(ring_pins), C_ST, 0.0], [camA, "#555b62", 60.0], [m0.ebA, "#9aa3ab", 105.0], [m0.discA, C_D, 105.0],
          [camB, "#555b62", 150.0], [m0.ebB, "#9aa3ab", 195.0], [m0.discB, "#6b7178", 195.0], [m0.opins, "#d6a83a", 250.0], [m0.carrier, "#b0b8c0", 250.0],
          [bearing_out, "#9aa3ab", 300.0], [cover, C_A, 335.0], [flange, C_A, 365.0]];
render(ex.map(|e| #{shape: e[0].translate(vec3(0, 0, e[2])).rotate(vec3(0, 0, 0), vec3(0, 1, 0), 90.0), color: e[1]}),
       #{path: out + "gearbox-prototype-exploded.png", width: 2000, height: 900, eye: [260, -620, 260], target: [200, 0, 0], up: [0, 0, 1], fov: 34, background: "#f3f4f6"});
// half section through the axis (world x > 0 removed), seen from +X
let half = block(vec3(0, -400, -200), vec3(400, 400, 400));
let sec = [[housing, C_H], [compound(ring_pins), C_ST], [motor, C_M], [camA, "#555b62"], [camB, "#555b62"], [m0.ebA, "#9aa3ab"], [m0.ebB, "#9aa3ab"],
           [m0.discA, C_D], [m0.discB, "#6b7178"], [m0.opins, "#d6a83a"], [m0.carrier, "#b0b8c0"], [bearing_out, "#9aa3ab"], [cover, C_A], [flange, C_A], [lever, C_A], [stand, C_P]];
render(sec.map(|e| #{shape: world(e[0], H).cut([half]), color: e[1]}),
       #{path: out + "gearbox-prototype-section.png", width: 1600, height: 1200, eye: [420, -150, 200], target: [0, 0, 95], up: [0, 0, 1], fov: 30, background: "#f3f4f6"});
// discs and pins, housing see-through, from the output side
render([#{shape: housing, color: "#d9662a", opacity: 0.3}, #{shape: m0.discA, color: C_D}, #{shape: m0.discB, color: "#6b7178"}, #{shape: compound(ring_pins), color: C_ST},
        #{shape: m0.opins, color: "#d6a83a"}, #{shape: camA, color: "#555b62"}, #{shape: camB, color: "#555b62"}, #{shape: motor, color: C_M}],
       #{path: out + "gearbox-prototype-inside.png", width: 1200, height: 1000, view: "iso", background: "#f3f4f6"});
render([#{shape: coupon, color: "#d6a83a"}, #{shape: coupon_big.translate(vec3(48, 110, 0)), color: "#d6a83a"}],
       #{path: out + "tolerance-coupon.png", width: 1000, height: 900, eye: [200, -180, 260], target: [48, 60, 0], up: [0, 0, 1], fov: 30, background: "#f3f4f6"});

let summary = #{
    ratio: RATIO, input_to_output: "opposite directions",
    axis_height_mm: H, z_stack: #{disc_A: z_d1, disc_B: z_d2, ledge: z_ledge, bearing: z_b0, housing_top: z_top, output_face: z_out},
    printed_mass_g: total_print_g, lever_mass_g: lm.mass, lever_self_torque_Nm: lever_self_Nm,
    torque: #{motor_holding_Nm: 0.65, usable_fraction: 0.6, gearbox_efficiency: 0.75, expected_output_Nm: 0.65 * 0.6 * RATIO * 0.75,
              arm_J2_needs_at_gearbox_Nm: 14.4 / 2.0 / 0.95},
    static_checks: checks, mesh_checks: mesh, parts: bom,
};
write_json(out + "summary.json", summary);
#{checks: checks.filter(|c| c.overlap_mm3 > 0.5), mesh: mesh, printed_g: total_print_g, lever_self_Nm: lever_self_Nm}
