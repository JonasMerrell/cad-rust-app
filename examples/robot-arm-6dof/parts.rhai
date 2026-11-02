// A 6-axis robot arm: 500 mm reach, 1 kg payload (tool included), NEMA 17 steppers driving
// 3D-printed cycloidal gearboxes, and a magnetic tool changer with pogo pins for active tools.
//   rcad script examples/robot-arm-6dof/parts.rhai [output folder]
// Writes a STEP and an STL per part, the posed and stretched assemblies as STEP, renders, and
// summary.json (masses, joint torques against what each actuator gives, interference, BOM).
// Units mm. World: Z up. Zero pose: arm stretched out along +X, J2/J3/J5 axes along Y,
// J4/J6 along X. The wrist is spherical: J4, J5 and J6 meet at the wrist centre.
// Printed (PA-CF in a heated chamber): gearbox housings, cycloid discs, carriers, cams, covers,
// base housing, turntable/tower, shoulder and elbow hubs, elbow block, forearm, wrist link,
// tool changer plates. Bought or cut: 40x40x2 aluminium tube (upper arm, cut to length and
// drilled), 8 mm aluminium base plate (drilled), bearings, dowel pins, magnets, pogo pins, steppers.

let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };
let FILES = !(ARGS.len() > 1 && ARGS[1] == "quick");   // "quick": skip the per-part STEP/STL files
let TAU = 6.283185307179586;

fn zcyl(x, y, z0, r, h) { cylinder(vec3(x, y, z0), vec3(0, 0, 1), r, h) }

fn ring_holes(n, pcd, a0, z0, r, h) {
    let hs = [];
    for i in 0..n {
        let a = (a0 + 360.0 * i / n).to_radians();
        hs.push(zcyl(pcd / 2.0 * cos(a), pcd / 2.0 * sin(a), z0, r, h));
    }
    hs
}

fn ycyl(x, y0, z, r, h) { cylinder(vec3(x, y0, z), vec3(0, 1, 0), r, h) }

fn ring(z0, ri, ro, h) { zcyl(0, 0, z0, ro, h).cut([zcyl(0, 0, z0 - 1, ri, h + 2)]) }

// cycloid disc outline for N ring pins of radius Rr on radius R, eccentricity E (N - 1 lobes)
fn cyc_pts(R, Rr, E, N, steps) {
    let pts = [];
    for i in 0..steps {
        let t = 6.283185307179586 * i / steps;
        let psi = atan(sin((1.0 - N) * t), R / (E * N) - cos((1.0 - N) * t));
        let x = R * cos(t) - Rr * cos(t + psi) - E * cos(N * t);
        let y = -R * sin(t) + Rr * sin(t + psi) + E * sin(N * t);
        pts.push([x, y, 0.0]);
    }
    pts
}

// NEMA 17 stepper: mounting face z = 0, body below, Ø5 shaft up
fn nema17(len) {
    let body = block(vec3(-21.15, -21.15, -len), vec3(21.15, 21.15, 0)).chamfer(3.5);
    fuse([body, zcyl(0, 0, 0, 11, 2), zcyl(0, 0, 0, 2.5, 22)]).unify()
}

// ================================================================ cycloidal actuator
// Local frame: output axis +Z, motor face (housing rear face) at z = 0, motor below, output face at z.out.
// Housing rear face carries 6 M3 heat-set inserts on pcd_mount, the bracket bolts to it with the motor
// passing through. Ring pins are steel dowels half sunk in the housing wall, output pins steel dowels
// pressed into the carrier, two discs 180° apart on a double eccentric cam.
fn act_z(s) {
    let d1 = s.tr + 0.5;
    let d2 = d1 + s.d + 1.0;
    let b0 = d2 + s.d + 1.0;
    let top = b0 + s.bw;
    #{d1: d1, d2: d2, b0: b0, top: top, out: top + 6.0}
}

fn actuator(s) {
    let z = act_z(s);
    let N = s.N;
    let rr = s.od / 2.0;
    // housing
    let hc = [zcyl(0, 0, s.tr, s.R, z.b0 - s.tr + 0.01), zcyl(0, 0, z.b0, s.bod / 2.0, s.bw + 1), zcyl(0, 0, -1, 11.25, s.tr + 2)];
    for p in [[15.5, 15.5], [-15.5, 15.5], [15.5, -15.5], [-15.5, -15.5]] { hc.push(zcyl(p[0], p[1], -1, 1.7, s.tr + 2)); }
    for i in 0..N { let a = 6.283185307179586 * i / N; hc.push(zcyl(s.R * cos(a), s.R * sin(a), s.tr - 3, s.Rr, z.b0 - s.tr + 3.01)); }
    let cover_pcd = s.bod / 2.0 + rr;
    hc += ring_holes(6, s.pcd_mount, 0, -1, 2.0, 9) + ring_holes(6, cover_pcd, 30, z.top - 8, 2.0, 9);
    let housing = zcyl(0, 0, 0, rr, z.top).cut(hc);
    // discs
    let prof = offset2d(face(wire(cyc_pts(s.R, s.Rr, s.E, N, (N - 1) * 16), true)), -0.08)[0];
    // the second disc is the first turned half a lobe; its bore and output holes stay on the same pattern
    let blank = extrude(prof, vec3(0, 0, s.d));
    let holes = [zcyl(0, 0, -1, s.ebod / 2.0, s.d + 2)] + ring_holes(s.nout, 2.0 * s.rh, 0, -1, s.rp + s.E, s.d + 2);
    let disc1 = blank.cut(holes).translate(vec3(s.E, 0, z.d1));
    let disc2 = blank.rotate(vec3(0, 0, 0), vec3(0, 0, 1), 180.0 / (N - 1)).cut(holes).translate(vec3(-s.E, 0, z.d2));
    // output carrier: plate in the output bearing, flange through the cover, output pins back through the discs
    let fr = s.flange_od / 2.0;
    let carrier = fuse([zcyl(0, 0, z.b0, s.bid / 2.0, s.bw), zcyl(0, 0, z.top, fr, 6)]).unify()
        .cut([zcyl(0, 0, z.b0 - 1, 4, 20)] + ring_holes(s.nout, 2.0 * s.rh, 0, z.b0 - 1, s.rp, 9) + ring_holes(6, s.flange_od - 12.0, 30, z.out - 8, 2.0, 9));
    let cover = ring(z.top, fr + 1.0, rr, 3).cut(ring_holes(6, cover_pcd, 30, z.top - 1, 1.7, 5));
    // cam on the motor shaft: collar, two eccentrics, D-bore
    let cam = fuse([zcyl(0, 0, 2.5, 8, z.d1 - 2.5), zcyl(s.E, 0, z.d1, s.ebid / 2.0, s.d + 0.5), zcyl(-s.E, 0, z.d2 - 0.5, s.ebid / 2.0, s.d + 0.5)]).unify()
        .cut([zcyl(0, 0, 0, if s.belt { 4.0 } else { 2.55 }, z.d2 + s.d + 1)]);
    let pins = [];
    for i in 0..N { let a = 6.283185307179586 * i / N; pins.push(zcyl(s.R * cos(a), s.R * sin(a), s.tr - 3, s.Rr, z.b0 - s.tr + 2.5)); }
    let opins = [];
    for i in 0..s.nout { let a = 6.283185307179586 * i / s.nout; opins.push(zcyl(s.rh * cos(a), s.rh * sin(a), z.d1, s.rp, z.b0 - z.d1 + 8)); }
    let ob = ring(z.b0, s.bid / 2.0, s.bod / 2.0, s.bw);
    let eb1 = ring(0, s.ebid / 2.0, s.ebod / 2.0, s.ebw).translate(vec3(s.E, 0, z.d1 + (s.d - s.ebw) / 2.0));
    let eb2 = ring(0, s.ebid / 2.0, s.ebod / 2.0, s.ebw).translate(vec3(-s.E, 0, z.d2 + (s.d - s.ebw) / 2.0));
    #{housing: housing, disc1: disc1, disc2: disc2, carrier: carrier, cover: cover, cam: cam,
      pins: compound(pins), opins: compound(opins), bearings: compound([ob, eb1, eb2]), motor: if s.belt { () } else { nema17(s.motor_len) }, z: z}
}

// actuator specs
let L50 = #{name: "L50", od: 120.0, R: 48.0, Rr: 1.5, E: 0.8, N: 51, d: 8.0, tr: 6.0, bw: 10.0, bid: 70.0, bod: 90.0, ebid: 25.0, ebod: 37.0, ebw: 7.0,
            rh: 31.0, rp: 3.0, nout: 6, flange_od: 74.0, pcd_mount: 104.0, motor_len: 60.0, motor: "17HS24 (60 mm, 0.65 N·m)", motor_mass: 500.0, motor_torque: 0.65,
            bearings: "6814 (70x90x10), 2x 6805 (25x37x7)", pin: "51x dowel 3x21", belt: false};
let L50B = L50 + #{name: "L50", belt: true, belt_ratio: 2.0, bearings: "6814 (70x90x10), 2x 6805 (25x37x7), 2x 608 (input shaft)"};
let L30 = L50 + #{name: "L30", Rr: 2.5, E: 1.3, N: 31, motor_len: 48.0, motor: "17HS19 (48 mm, 0.45 N·m)", motor_mass: 360.0, motor_torque: 0.45, pin: "31x dowel 5x21"};
let M30 = #{name: "M30", od: 100.0, R: 38.0, Rr: 2.0, E: 1.0, N: 31, d: 7.0, tr: 6.0, bw: 10.0, bid: 60.0, bod: 78.0, ebid: 20.0, ebod: 32.0, ebw: 7.0,
            rh: 25.0, rp: 2.5, nout: 6, flange_od: 64.0, pcd_mount: 88.0, motor_len: 60.0, motor: "17HS24 (60 mm, 0.65 N·m)", motor_mass: 500.0, motor_torque: 0.65,
            bearings: "6812 (60x78x10), 2x 6804 (20x32x7)", pin: "31x dowel 4x19", belt: false};
let S20 = #{name: "S20", od: 80.0, R: 30.0, Rr: 2.0, E: 1.2, N: 21, d: 6.0, tr: 5.0, bw: 7.0, bid: 50.0, bod: 65.0, ebid: 17.0, ebod: 26.0, ebw: 5.0,
            rh: 20.0, rp: 2.0, nout: 6, flange_od: 54.0, pcd_mount: 70.0, motor_len: 40.0, motor: "17HS16 (40 mm, 0.40 N·m)", motor_mass: 280.0, motor_torque: 0.40,
            bearings: "6810 (50x65x7), 2x 6803 (17x26x5)", pin: "21x dowel 4x17", belt: false};
let XS20 = #{name: "XS20", od: 72.0, R: 27.0, Rr: 2.0, E: 1.0, N: 21, d: 6.0, tr: 5.0, bw: 7.0, bid: 40.0, bod: 52.0, ebid: 17.0, ebod: 26.0, ebw: 5.0,
            rh: 17.0, rp: 1.5, nout: 6, flange_od: 43.0, pcd_mount: 64.0, motor_len: 34.0, motor: "17HS13 (34 mm, 0.28 N·m)", motor_mass: 220.0, motor_torque: 0.28,
            bearings: "6808 (40x52x7), 2x 6803 (17x26x5)", pin: "21x dowel 4x17", belt: false};
let XS20P = XS20 + #{name: "XS20P", motor_len: 23.0, motor: "17HS4023 pancake (23 mm, 0.13 N·m)", motor_mass: 140.0, motor_torque: 0.13};

let joints = [
    #{j: "J1", spec: L30, ratio: 30.0},
    #{j: "J2", spec: L50B, ratio: 100.0},   // 2:1 GT2 belt into the 50:1 cycloid
    #{j: "J3", spec: M30, ratio: 30.0},
    #{j: "J4", spec: S20, ratio: 20.0},
    #{j: "J5", spec: XS20, ratio: 20.0},
    #{j: "J6", spec: XS20P, ratio: 20.0},
];

// ================================================================ layout (zero pose)
let L1 = 240.0;            // J2 -> J3
let L2 = 190.5;            // J3 -> wrist centre
let zB = 98.0;             // top of the base housing = J1 rear face
let z1o = zB + act_z(L30).out;   // J1 output face
let tt = 10.0;             // turntable thickness
let H2 = 222.0;            // J2 axis height
let wH = 25.0;             // half width (Y) of the shoulder/elbow hubs, arm plane y = 0
let yR = wH + act_z(L50).out;    // J2 housing rear face (on the tower)
let yJ3o = wH + act_z(M30).out;  // J3 output face
let yF = yJ3o + 42.0;      // J4 / forearm / wrist axis
let xJ4 = L1 + 44.0;       // J4 rear face
let xJ4o = xJ4 + act_z(S20).out;
let W = L1 + L2;           // wrist centre x
let yJ5r = yF - 79.0;      // J5 rear face
let yJ5o = yJ5r + act_z(XS20).out;
let xJ6 = W + 25.0;        // J6 rear face
let xJ6o = xJ6 + act_z(XS20P).out;
let xT = xJ6o + 12.0;      // tool changer interface face
let reach = xT;            // from the J2 axis

// placement of each actuator: local +Z to world direction, origin
// rot as [axis, deg]
let act_place = #{
    J1: #{rot: [[0, 0, 1], 0.0], pos: [0.0, 0.0, zB]},
    J2: #{rot: [[1, 0, 0], 90.0], pos: [0.0, yR, H2]},       // +Z -> -Y
    J3: #{rot: [[1, 0, 0], -90.0], pos: [L1, wH, H2]},       // +Z -> +Y
    J4: #{rot: [[0, 1, 0], 90.0], pos: [xJ4, yF, H2]},       // +Z -> +X
    J5: #{rot: [[1, 0, 0], -90.0], pos: [W, yJ5r, H2]},      // +Z -> +Y
    J6: #{rot: [[0, 1, 0], 90.0], pos: [xJ6, yF, H2]},
};
fn place(s, p) { s.rotate(vec3(0, 0, 0), p.rot[0], p.rot[1]).translate(p.pos) }

// ================================================================ structure (printed unless noted)
// base plate, 8 mm aluminium, drilled only: 4 table holes, 6 housing screws, cable hole
let base_plate = block(vec3(-110, -110, 0), vec3(110, 110, 8)).cut(
    ring_holes(4, 268.7, 45, -1, 3.3, 10) + ring_holes(6, 150, 0, -1, 2.2, 10) + [zcyl(-60, 0, -1, 8, 10)]);
base_plate = base_plate.fillet(base_plate.edges().filter(|e| e.kind() == "line" && abs(e.start().z - e.end().z) > 1.0), 10);

// base housing Ø170: wall 4, inner lip at the bottom for the plate screws, 8 mm deck carrying J1
let bh_cuts = [zcyl(0, 0, 12, 81, zB - 8.0 - 12.0), zcyl(0, 0, 7, 69, 6), block(vec3(-21.9, -21.9, zB - 9.0), vec3(21.9, 21.9, zB + 1.0)),
    block(vec3(-90, -22, 14), vec3(-70, 22, 40))]
    + ring_holes(6, 150, 0, 7, 2.0, 9) + ring_holes(6, 104, 0, zB - 9.0, 1.7, 10) + ring_holes(6, 104, 0, zB - 9.0, 3.2, 4.0);
let base_housing = zcyl(0, 0, 8, 85, zB - 8.0).cut(bh_cuts);

// turntable + tower + belt bay: disc on J1, plate carrying J2, then a 25 mm bay for the 2:1 GT2 belt
// and an outrigger plate carrying the J2 motor (behind the axis, shaft pointing back into the bay).
// The J2 input is an Ø8 shaft from the cam through 608 bearings in the tower and the outrigger.
let tw_y0 = yR;  let tw_y1 = yR + 10.0;
let or_y0 = tw_y1 + 25.0;  let or_y1 = or_y0 + 8.0;
let xM2 = -58.0;           // J2 motor axis, 58 mm behind J2 (GT2 20T -> 40T)
let tt_z0 = z1o;  let tt_z1 = z1o + tt;
let ttable = fuse([zcyl(0, 0, tt_z0, 60, tt), block(vec3(-92, -20, tt_z0), vec3(60, or_y1, tt_z1))]).unify();
let tower = fuse([block(vec3(-92, tw_y0, tt_z1 - 1.0), vec3(60, tw_y1, H2)), cylinder(vec3(0, tw_y0, H2), vec3(0, 1, 0), 62, 10)]).unify();
let outrigger = block(vec3(-92, or_y0, tt_z1 - 1.0), vec3(52, or_y1, H2 + 30.0));
let walls = [block(vec3(-92, tw_y1 - 1.0, tt_z1 - 1.0), vec3(-84, or_y0 + 1.0, H2 + 30.0)), block(vec3(44, tw_y1 - 1.0, tt_z1 - 1.0), vec3(52, or_y0 + 1.0, H2 + 30.0))];
let tw_cuts = [ycyl(0, tw_y0 - 1.0, H2, 6, 12), ycyl(0, tw_y1 - 7.0, H2, 11, 8), ycyl(0, or_y0 - 1.0, H2, 5, 10), ycyl(0, or_y0 - 1.0, H2, 11, 8),
               ycyl(xM2, or_y0 - 1.0, H2, 11.25, 10)];
for p in [[15.5, 15.5], [-15.5, 15.5], [15.5, -15.5], [-15.5, -15.5]] { tw_cuts.push(ycyl(xM2 + p[0], or_y0 - 1.0, H2 + p[1], 1.7, 10)); }
for i in 0..6 {
    let a = (60.0 * i).to_radians();
    tw_cuts.push(ycyl(52 * cos(a), tw_y0 - 1.0, H2 + 52 * sin(a), 1.7, 12));
    tw_cuts.push(ycyl(52 * cos(a), tw_y1 - 3.0, H2 + 52 * sin(a), 3.2, 4));
}
for i in 0..6 { let a = (30.0 + 60.0 * i).to_radians(); tw_cuts.push(zcyl(31 * cos(a), 31 * sin(a), tt_z0 - 1.0, 1.7, tt + 2.0)); tw_cuts.push(zcyl(31 * cos(a), 31 * sin(a), tt_z1 - 3.5, 3.2, 4)); }
tw_cuts.push(zcyl(0, 0, tt_z0 - 1.0, 8, tt + 2.0));
let turntable = fuse([ttable, tower, outrigger] + walls).unify().cut(tw_cuts);

// belt stage: pulleys, belt loop, input shaft, motor
fn loop_xz(c1, r1, c2, r2, y0, w) {
    let pts = [];
    for i in 0..48 { let a = 6.283185307179586 * i / 48; pts.push([c1[0] + r1 * cos(a), c1[1] + r1 * sin(a)]); pts.push([c2[0] + r2 * cos(a), c2[1] + r2 * sin(a)]); }
    let h = convex_hull(pts);
    extrude(face(wire(h.map(|p| [p.x, y0, p.y]), true)), vec3(0, w, 0))
}
let by = tw_y1 + 9.0;   // belt plane start (6 mm GT2)
let belt = loop_xz([0.0, H2], 13.5, [xM2, H2], 7.1, by, 6).cut([loop_xz([0.0, H2], 12.73, [xM2, H2], 6.37, by - 1.0, 8)]);
let pulley40 = fuse([ycyl(0, by - 1.5, H2, 14.5, 1.5), ycyl(0, by, H2, 12.73, 6), ycyl(0, by + 6.0, H2, 14.5, 1.5), ycyl(0, by + 7.5, H2, 8, 7)]).unify().cut([ycyl(0, by - 3.0, H2, 4, 20)]);
let pulley20 = fuse([ycyl(xM2, by - 1.5, H2, 8, 1.5), ycyl(xM2, by, H2, 6.37, 6), ycyl(xM2, by + 6.0, H2, 8, 1.5), ycyl(xM2, by + 7.5, H2, 7, 7)]).unify().cut([ycyl(xM2, by - 3.0, H2, 2.5, 20)]);
let j2_shaft = ycyl(0, yR - 23.0, H2, 4, or_y1 + 2.0 - (yR - 23.0));
let j2_motor = nema17(60.0).rotate(vec3(0, 0, 0), vec3(1, 0, 0), 90.0).translate(vec3(xM2, or_y1, H2));
let j2_608 = compound([ycyl(0, tw_y1 - 7.0, H2, 11, 7).cut([ycyl(0, tw_y1 - 8.0, H2, 4, 9)]), ycyl(0, or_y0, H2, 11, 7).cut([ycyl(0, or_y0 - 1.0, H2, 4, 9)])]);

// upper arm: 40x40x2 aluminium tube between a shoulder hub (on J2) and an elbow hub (carries J3)
let TS = 40.4;
let tube_x0 = 30.0;  let tube_x1 = L1 - 30.0;
let upper_tube = block(vec3(tube_x0, -20, H2 - 20.0), vec3(tube_x1, 20, H2 + 20.0)).cut([block(vec3(tube_x0 - 1.0, -18, H2 - 18.0), vec3(tube_x1 + 1.0, 18, H2 + 18.0))]
    + [cylinder(vec3(45, -30, H2), vec3(0, 1, 0), 2.15, 60), cylinder(vec3(65, -30, H2), vec3(0, 1, 0), 2.15, 60),
       cylinder(vec3(L1 - 45.0, -30, H2), vec3(0, 1, 0), 2.15, 60), cylinder(vec3(L1 - 65.0, -30, H2), vec3(0, 1, 0), 2.15, 60)]);


// shoulder hub: disc around J2, socket for the tube along +X, J2 carrier screws from the -Y side
let sh_cuts = [block(vec3(tube_x0, -TS / 2.0, H2 - TS / 2.0), vec3(80, TS / 2.0, H2 + TS / 2.0)), ycyl(0, -wH - 1.0, H2, 8, 2.0 * wH + 2.0),
               ycyl(45, -wH - 1.0, H2, 2.15, 2.0 * wH + 2.0), ycyl(65, -wH - 1.0, H2, 2.15, 2.0 * wH + 2.0)];
for i in 0..6 {
    let a = (30.0 + 60.0 * i).to_radians();
    sh_cuts.push(ycyl(31 * cos(a), -wH - 1.0, H2 + 31 * sin(a), 1.7, 2.0 * wH + 2.0));
    sh_cuts.push(ycyl(31 * cos(a), -wH - 1.0, H2 + 31 * sin(a), 3.2, 2.0 * wH - 6.0));
}
let shoulder_hub = fuse([ycyl(0, -wH, H2, 40, 2.0 * wH), block(vec3(0, -wH, H2 - 28.0), vec3(75, wH, H2 + 28.0))]).unify().cut(sh_cuts);

// elbow hub: disc around J3 with the J3 motor pocket, socket for the tube along -X; J3 bolts to its +Y face
let eh_cuts = [block(vec3(L1 - 86.0, -TS / 2.0, H2 - TS / 2.0), vec3(L1 - 30.0, TS / 2.0, H2 + TS / 2.0)),
               block(vec3(L1 - 21.9, -wH - 1.0, H2 - 21.9), vec3(L1 + 21.9, wH + 1.0, H2 + 21.9)),
               ycyl(L1 - 45.0, -wH - 1.0, H2, 2.15, 2.0 * wH + 2.0), ycyl(L1 - 65.0, -wH - 1.0, H2, 2.15, 2.0 * wH + 2.0)];
for i in 0..6 {
    let a = (60.0 * i).to_radians();
    eh_cuts.push(ycyl(L1 + 44 * cos(a), -wH - 1.0, H2 + 44 * sin(a), 1.7, 2.0 * wH + 2.0));
    eh_cuts.push(ycyl(L1 + 44 * cos(a), -wH - 1.0, H2 + 44 * sin(a), 3.2, 2.0 * wH - 6.0));
}
let elbow_hub = fuse([ycyl(L1, -wH, H2, 52, 2.0 * wH), block(vec3(L1 - 85.0, -wH, H2 - 28.0), vec3(L1, wH, H2 + 28.0))]).unify().cut(eh_cuts);

// elbow block: plate on the J3 carrier, plate across X carrying J4 (motor through it), bottom web
let eb_y0 = yJ3o;  let eb_y1 = yF + 42.0;
let eb_cuts = [block(vec3(xJ4 - 11.0, yF - 21.9, H2 - 21.9), vec3(xJ4 + 1.0, yF + 21.9, H2 + 21.9)), ycyl(L1, eb_y0 - 1.0, H2, 8, 12)];
for i in 0..6 {
    let a = (30.0 + 60.0 * i).to_radians();
    eb_cuts.push(ycyl(L1 + 26 * cos(a), eb_y0 - 1.0, H2 + 26 * sin(a), 1.7, 12));
    let b = (30.0 + 60.0 * i).to_radians();   // J4 housing inserts (local 60i) after RY(90)
    eb_cuts.push(cylinder(vec3(xJ4 + 1.0, yF + 35 * cos(b), H2 + 35 * sin(b)), vec3(-1, 0, 0), 1.7, 12));
    eb_cuts.push(cylinder(vec3(xJ4 - 7.0, yF + 35 * cos(b), H2 + 35 * sin(b)), vec3(-1, 0, 0), 3.2, 5));
}
let elbow_block = fuse([
    fuse([ycyl(L1, eb_y0, H2, 42, 10), block(vec3(L1, eb_y0, H2 - 42.0), vec3(xJ4, eb_y0 + 10.0, H2 + 42.0))]).unify(),
    block(vec3(xJ4 - 10.0, eb_y0, H2 - 42.0), vec3(xJ4, eb_y1, H2 + 42.0)),
    block(vec3(L1 + 10.0, eb_y0, H2 - 42.0), vec3(xJ4, eb_y1, H2 - 34.0)),
    block(vec3(L1 + 10.0, eb_y0, H2 + 34.0), vec3(xJ4, eb_y1, H2 + 42.0)),
]).unify().cut(eb_cuts);

// forearm, one print: flange on the J4 carrier, hollow box, bridge across to the J5 cheek
let xa = W - 55.0;
let fcheek_y0 = yJ5r - 10.0;
let fa_cuts = [block(vec3(xJ4o + 8.0, yF - 23.0, H2 - 23.0), vec3(xa - 50.0, yF + 23.0, H2 + 23.0)),
               block(vec3(W - 21.9, fcheek_y0 - 1.0, H2 - 21.9), vec3(W + 21.9, yJ5r + 1.0, H2 + 21.9)),
               cylinder(vec3(xJ4o - 1.0, yF, H2), vec3(1, 0, 0), 6, 12)];
for i in 0..6 {
    let a = (60.0 * i).to_radians();   // J4 carrier holes (local 30° + 60i) after RY(90)
    fa_cuts.push(cylinder(vec3(xJ4o - 1.0, yF + 21 * cos(a), H2 + 21 * sin(a)), vec3(1, 0, 0), 1.7, 12));
    fa_cuts.push(cylinder(vec3(xJ4o + 4.0, yF + 21 * cos(a), H2 + 21 * sin(a)), vec3(1, 0, 0), 3.2, 20));
    let b = (60.0 * i).to_radians();
    fa_cuts.push(ycyl(W + 32 * cos(b), fcheek_y0 - 1.0, H2 + 32 * sin(b), 1.7, 12));
    fa_cuts.push(ycyl(W + 32 * cos(b), fcheek_y0 - 1.0, H2 + 32 * sin(b), 3.2, 4));
}
let cheek_pts = [];
for i in 0..24 { let a = TAU * i / 24; cheek_pts.push([W + 40.0 * cos(a), H2 + 40.0 * sin(a)]); }
for p in [[xa - 50.0, H2 - 28.0], [xa - 50.0, H2 + 28.0]] { cheek_pts.push(p); }
let hull = convex_hull(cheek_pts);
let cheek_face = face(wire(hull.map(|p| [p.x, fcheek_y0, p.y]), true));
let forearm = fuse([
    cylinder(vec3(xJ4o, yF, H2), vec3(1, 0, 0), 30, 8),
    block(vec3(xJ4o, yF - 28.0, H2 - 28.0), vec3(xa, yF + 28.0, H2 + 28.0)),
    block(vec3(xa - 50.0, fcheek_y0, H2 - 28.0), vec3(xa, yF - 23.0, H2 + 28.0)),
    extrude(cheek_face, vec3(0, 10, 0)),
]).unify().cut(fa_cuts);

// wrist link: plate on the J5 carrier, plate across X carrying J6 (pancake motor through it)
let wl_y0 = yJ5o;  let wl_y1 = yF + 38.0;
let wl_cuts = [block(vec3(xJ6 - 9.0, yF - 21.9, H2 - 21.9), vec3(xJ6 + 1.0, yF + 21.9, H2 + 21.9)), ycyl(W, wl_y0 - 1.0, H2, 5, 10)];
for i in 0..6 {
    let a = (30.0 + 60.0 * i).to_radians();
    wl_cuts.push(ycyl(W + 15.5 * cos(a), wl_y0 - 1.0, H2 + 15.5 * sin(a), 1.7, 10));
    let b = (30.0 + 60.0 * i).to_radians();   // J6 housing inserts after RY(90)
    wl_cuts.push(cylinder(vec3(xJ6 + 1.0, yF + 32 * cos(b), H2 + 32 * sin(b)), vec3(-1, 0, 0), 1.7, 10));
}
let wrist_link = fuse([
    fuse([ycyl(W, wl_y0, H2, 28, 8), block(vec3(W, wl_y0, H2 - 28.0), vec3(xJ6, wl_y0 + 8.0, H2 + 28.0))]).unify(),
    block(vec3(xJ6 - 8.0, wl_y0, H2 - 38.0), vec3(xJ6, wl_y1, H2 + 38.0)),
]).unify().cut(wl_cuts);

// ================================================================ tool changer
// Arm side: Ø68 x 12 on the J6 carrier. Kinematic coupling: three V-grooves made of pairs of Ø4 dowels
// (at 90°, 210°, 330°) take three Ø8 balls on the tool; three Ø10x3 N52 magnets hold it; an 8-pin
// 2.54 mm spring-loaded pogo connector in the middle meets gold pads on the tool's PCB.
// Pins: 24V, 24V, GND, GND, CAN-H, CAN-L, TOOL_DETECT (tool ties it to GND), spare.
fn xcyl(x0, y, z, r, h) { cylinder(vec3(x0, y, z), vec3(1, 0, 0), r, h) }
let tc_cuts = [block(vec3(xT - 8.0, yF - 6.0, H2 - 3.5), vec3(xT + 1.0, yF + 6.0, H2 + 3.5)), xcyl(xJ6o - 1.0, yF, H2, 3.0, 14)];
let vpins = [];
let balls = [];
for k in 0..3 {
    let a = (90.0 + 120.0 * k).to_radians();
    let u = vec3(0, cos(a), sin(a));          // radial
    let v = vec3(0, -sin(a), cos(a));         // tangential
    let c = vec3(xT, yF, H2) + u * 24.0;
    // slot for the two pins, 9.2 wide, 2.5 deep
    let slot = block(vec3(-2.5, -6.0, -4.6), vec3(0.01, 6.0, 4.6)).rotate(vec3(0, 0, 0), vec3(1, 0, 0), (90.0 + 120.0 * k)).translate(c);
    tc_cuts.push(slot);
    for sgn in [-1.0, 1.0] {
        let pc = c + v * (2.6 * sgn) - u * 6.0;
        vpins.push(cylinder(vec3(pc.x - 0.5, pc.y, pc.z), u, 2.0, 12));
    }
    balls.push(sphere_at(vec3(xT + 5.41, c.y, c.z), 4.0));
    let m = (30.0 + 120.0 * k).to_radians();
    tc_cuts.push(xcyl(xT - 3.0, yF + 22 * cos(m), H2 + 22 * sin(m), 5.1, 4));
}
for i in 0..6 {
    let a = (60.0 * i).to_radians();   // J6 carrier holes after RY(90), clear of the magnets and grooves
    tc_cuts.push(xcyl(xJ6o - 1.0, yF + 15.5 * cos(a), H2 + 15.5 * sin(a), 1.7, 14));
    tc_cuts.push(xcyl(xT - 3.5, yF + 15.5 * cos(a), H2 + 15.5 * sin(a), 3.0, 4));
}
let tc_arm = xcyl(xJ6o, yF, H2, 34, 12).cut(tc_cuts);
let pogo = fuse([block(vec3(xT - 8.0, yF - 5.1, H2 - 2.55), vec3(xT, yF + 5.1, H2 + 2.55))]
    + [-3.81, -1.27, 1.27, 3.81].map(|y| [-1.27, 1.27].map(|z| xcyl(xT - 1.0, yF + y, H2 + z, 0.5, 2.5))).reduce(|a, b| a + b, [])).unify();
let magnets = [];
for k in 0..3 { let m = (30.0 + 120.0 * k).to_radians(); magnets.push(xcyl(xT - 3.0, yF + 22 * cos(m), H2 + 22 * sin(m), 5.0, 3)); }

// tool side: Ø68 x 10 plate 1 mm off the face, recesses over the pins, ball pockets, magnet pockets, pad PCB
let xTt = xT + 1.0;
let tp_cuts = [block(vec3(xTt - 1.0, yF - 7.5, H2 - 5.0), vec3(xTt + 1.6, yF + 7.5, H2 + 5.0))];
for k in 0..3 {
    let a = (90.0 + 120.0 * k).to_radians();
    let c = vec3(xTt, yF + 24.0 * cos(a), H2 + 24.0 * sin(a));
    tp_cuts.push(block(vec3(-1.0, -7.0, -5.0), vec3(3.0, 7.0, 5.0)).rotate(vec3(0, 0, 0), vec3(1, 0, 0), (90.0 + 120.0 * k)).translate(c));
    tp_cuts.push(xcyl(xTt + 2.0, c.y, c.z, 4.0, 6));
    let m = (30.0 + 120.0 * k).to_radians();
    tp_cuts.push(xcyl(xTt - 1.0, yF + 22 * cos(m), H2 + 22 * sin(m), 5.1, 4));
}
let tool_plate = xcyl(xTt, yF, H2, 34, 10).cut(tp_cuts);
let pad_pcb = block(vec3(xTt, yF - 7.0, H2 - 4.5), vec3(xTt + 1.6, yF + 7.0, H2 + 4.5));
let tool_magnets = [];
for k in 0..3 { let m = (30.0 + 120.0 * k).to_radians(); tool_magnets.push(xcyl(xTt, yF + 22 * cos(m), H2 + 22 * sin(m), 5.0, 3)); }

// example tool: a servo gripper body and two fingers (its mass is part of the 1 kg)
let tb_x0 = xTt + 10.0;
let tool_body = fuse([block(vec3(tb_x0, yF - 25.0, H2 - 30.0), vec3(tb_x0 + 45.0, yF + 25.0, H2 + 30.0)).fillet(4)]);
let fingers = [];
for s in [-1.0, 1.0] {
    fingers.push(block(vec3(tb_x0 + 45.0, yF - 6.0, H2 + s * 8.0 - 4.0), vec3(tb_x0 + 95.0, yF + 6.0, H2 + s * 8.0 + 4.0)));
}
let tool_fingers = fuse(fingers);

// controller board stand-in inside the base: ESP32-S3, 6x TMC2209, CAN transceiver, I2C mux
let controller = fuse([block(vec3(-45, -40, 16), vec3(45, 40, 17.6)), block(vec3(-40, -30, 17.6), vec3(-15, 30, 30)), block(vec3(-5, -35, 17.6), vec3(40, 35, 28))]);

// ================================================================ actuators, placed
let act = #{};
for jn in joints { act[jn.j] = actuator(jn.spec); }

// ================================================================ parts list
// group: which joint moves it (base, J1 ... J6, tool). kind: printed / bought / cut
let parts = [];
fn add(name, shape, group, kind, material, color) { this.push(#{name: name, shape: shape, group: group, kind: kind, material: material, color: color}); }
let C_P = "#e8742a";  let C_D = "#3a3f45";  let C_AL = "#c4ccd4";  let C_M = "#2b2f33";  let C_ST = "#7d858c";
parts.add("Base plate", base_plate, "base", "cut", "Aluminium 6061", C_AL);
parts.add("Base housing", base_housing, "base", "printed", "PA-CF", C_P);
parts.add("Controller board", controller, "base", "bought", "PCB", "#1f6e3a");
parts.add("Turntable", turntable, "J1", "printed", "PA-CF", C_P);
parts.add("J2 L50 motor", j2_motor, "J1", "bought", "motor", C_M);
parts.add("J2 GT2 pulley 40T", pulley40, "J1", "bought", "Aluminium 6061", "#d6a83a");
parts.add("J2 GT2 pulley 20T", pulley20, "J1", "bought", "Aluminium 6061", "#d6a83a");
parts.add("J2 GT2 belt 6 mm", belt, "J1", "bought", "PCB", "#202326");
parts.add("J2 input shaft 8 mm", j2_shaft, "J1", "cut", "Steel", C_ST);
parts.add("J2 input bearings 608", j2_608, "J1", "bought", "Steel", C_ST);
parts.add("Shoulder hub", shoulder_hub, "J2", "printed", "PA-CF", C_P);
parts.add("Upper arm tube 40x40x2", upper_tube, "J2", "cut", "Aluminium 6061", C_AL);
parts.add("Elbow hub", elbow_hub, "J2", "printed", "PA-CF", C_P);
parts.add("Elbow block", elbow_block, "J3", "printed", "PA-CF", C_P);
parts.add("Forearm", forearm, "J4", "printed", "PA-CF", C_P);
parts.add("Wrist link", wrist_link, "J5", "printed", "PA-CF", C_P);
parts.add("Tool changer (arm side)", tc_arm, "J6", "printed", "PA-CF", C_D);
parts.add("V-groove dowels 4x12", compound(vpins), "J6", "bought", "Steel", C_ST);
parts.add("Magnets 10x3 (arm)", compound(magnets), "J6", "bought", "NdFeB", "#9aa3ab");
parts.add("Pogo connector 8-pin", pogo, "J6", "bought", "Brass", "#d6a83a");
parts.add("Tool plate", tool_plate, "tool", "printed", "PA-CF", C_D);
parts.add("Coupling balls 8", compound(balls), "tool", "bought", "Steel", C_ST);
parts.add("Magnets 10x3 (tool)", compound(tool_magnets), "tool", "bought", "NdFeB", "#9aa3ab");
parts.add("Tool pad PCB", pad_pcb, "tool", "bought", "PCB", "#1f6e3a");
parts.add("Example tool body", tool_body, "tool", "printed", "PA-CF", "#4a7bd0");
parts.add("Example tool fingers", tool_fingers, "tool", "printed", "PA-CF", "#2c3e60");

// an actuator's stator (housing, cover, pins, bearings outer, motor) moves with the link it is bolted to,
// its output side (carrier, output pins) with the next link. Mount links: J1 on base, J2 on J1, J3 on J2,
// J4 on J3, J5 on J4, J6 on J5.
let mount_of = #{J1: "base", J2: "J1", J3: "J2", J4: "J3", J5: "J4", J6: "J5"};
for jn in joints {
    let a = act[jn.j];  let p = act_place[jn.j];  let n = jn.j + " " + jn.spec.name;  let g = mount_of[jn.j];
    parts.add(n + " housing", place(a.housing, p), g, "printed", "PA-CF", "#d9662a");
    parts.add(n + " cover", place(a.cover, p), g, "printed", "PA-CF", "#d9662a");
    parts.add(n + " cycloid disc A", place(a.disc1, p), g, "printed", "PA-CF", C_D);
    parts.add(n + " cycloid disc B", place(a.disc2, p), g, "printed", "PA-CF", C_D);
    parts.add(n + " cam", place(a.cam, p), g, "printed", "PA-CF", "#555b62");
    parts.add(n + " ring pins", place(a.pins, p), g, "bought", "Steel", C_ST);
    parts.add(n + " bearings", place(a.bearings, p), g, "bought", "Steel", C_ST);
    if type_of(a.motor) != "()" { parts.add(n + " motor", place(a.motor, p), g, "bought", "motor", C_M); }
    parts.add(n + " carrier", place(a.carrier, p), jn.j, "printed", "PA-CF", "#b0b8c0");
    parts.add(n + " output pins", place(a.opins, p), jn.j, "bought", "Steel", C_ST);
}

// ================================================================ mass and torque
// PA-CF printed with 4 walls and 40 % gyroid comes out near 65 % of solid; bearings ~70 % of a solid ring
let density = #{"PA-CF": 1.20e-3 * 0.65, "Aluminium 6061": 2.70e-3, "Steel": 7.85e-3, "NdFeB": 7.5e-3, "Brass": 8.5e-3, "PCB": 1.9e-3};
let motor_mass = #{};
for jn in joints { motor_mass[jn.j + " " + jn.spec.name + " motor"] = jn.spec.motor_mass; }
let order = ["base", "J1", "J2", "J3", "J4", "J5", "J6", "tool"];
let group_mass = #{};
for g in order { group_mass[g] = []; }
let total = 0.0;
for i in 0..parts.len() {
    let p = parts[i];
    if p.group == "tool" { continue; }          // the tool is inside the 1 kg payload
    let m = if p.material == "motor" { point_mass(motor_mass[p.name], p.shape.centroid()) }
            else if p.name.ends_with("bearings") { mass_props(p.shape, 7.85e-3 * 0.7) }
            else { mass_props(p.shape, density[p.material]) };
    parts[i].mass = m.mass;
    group_mass[p.group].push(m);
    total += m.mass;
}
let PAYLOAD = 1000.0;
let tool_cg = vec3(xT + 45.0, yF, H2);        // payload centre, 45 mm out from the interface face
group_mass["tool"].push(point_mass(PAYLOAD, tool_cg));

// moving mass beyond each joint, stretched out (worst case for J2, J3, J5)
fn beyond(gm, order, k) { let ms = []; for i in k..order.len() { ms += gm[order[i]]; } combine(ms) }
let g = 9.81e-3;
let m2 = beyond(group_mass, order, 2);  // everything J2 lifts
let m3 = beyond(group_mass, order, 3);
let m5 = beyond(group_mass, order, 5);
let m4 = beyond(group_mass, order, 4);
let t = #{
    J2: m2.mass * g * m2.cg.x / 1000.0,
    J3: m3.mass * g * (m3.cg.x - L1) / 1000.0,
    J5: m5.mass * g * (m5.cg.x - W) / 1000.0,
    // J4 with the wrist pitched 90°: the wrist group's lever about the forearm axis
    J4: m5.mass * g * (m5.cg.x - W) / 1000.0,
    J6: PAYLOAD * g * 30.0 / 1000.0,   // allowance: payload 30 mm off the tool axis
};
// J1 holds no gravity load; size it for accelerating the arm: 90°/s² ... use 1.5 rad/s² about Z
let m1 = beyond(group_mass, order, 1);
t.J1 = 0.0;
// usable motor torque: 60 % of holding torque at working speed (TMC2209, 24 V), gearbox efficiency 75 %
let eff = 0.75;  let usable = 0.6;
let torque = #{};
for jn in joints {
    let avail = jn.spec.motor_torque * usable * jn.ratio * eff * (if jn.spec.belt { 0.95 } else { 1.0 });
    let need = t[jn.j];
    torque[jn.j] = #{gearbox: jn.spec.name, ratio: jn.ratio, motor: jn.spec.motor, available_Nm: avail, static_need_Nm: need,
                     margin: if need > 0.0 { avail / need } else { () }};
}

// ================================================================ pose, interference
// pose angles (deg) for the render; rotation about each joint's zero-pose axis, distal first
let axes = #{J1: [[0, 0, 0], [0, 0, 1]], J2: [[0, 0, H2], [0, 1, 0]], J3: [[L1, 0, H2], [0, 1, 0]],
             J4: [[0, yF, H2], [1, 0, 0]], J5: [[W, 0, H2], [0, 1, 0]], J6: [[0, yF, H2], [1, 0, 0]]};
let pose = #{J1: 30.0, J2: -55.0, J3: 80.0, J4: 0.0, J5: 50.0, J6: 0.0};
fn posed(s, group, pose, axes) {
    let chain = #{base: [], J1: ["J1"], J2: ["J2", "J1"], J3: ["J3", "J2", "J1"], J4: ["J4", "J3", "J2", "J1"],
                  J5: ["J5", "J4", "J3", "J2", "J1"], J6: ["J6", "J5", "J4", "J3", "J2", "J1"], tool: ["J6", "J5", "J4", "J3", "J2", "J1"]};
    let r = s;
    for j in chain[group] { if pose[j] != 0.0 { r = r.rotate(axes[j][0], axes[j][1], pose[j]); } }
    r
}

let checks = [];
fn find(parts, name) { for p in parts { if p.name == name { return p.shape; } } () }
for pair in [["Turntable", "J2 L50 housing"], ["Turntable", "J2 L50 motor"], ["Turntable", "J2 GT2 pulley 40T"], ["Turntable", "J2 GT2 pulley 20T"], ["Turntable", "J2 GT2 belt 6 mm"], ["J2 L50 cam", "J2 input shaft 8 mm"], ["Turntable", "J2 input shaft 8 mm"], ["Shoulder hub", "J2 L50 carrier"], ["Shoulder hub", "J2 L50 housing"],
             ["Elbow hub", "J3 M30 housing"], ["Elbow hub", "J3 M30 motor"], ["Upper arm tube 40x40x2", "Elbow hub"], ["Elbow block", "J3 M30 housing"],
             ["Elbow block", "J4 S20 motor"], ["Elbow block", "J4 S20 housing"], ["Forearm", "J4 S20 housing"], ["Forearm", "J5 XS20 housing"],
             ["Forearm", "J5 XS20 motor"], ["Wrist link", "J5 XS20 housing"], ["Wrist link", "J6 XS20P motor"], ["Wrist link", "J6 XS20P housing"],
             ["Forearm", "J6 XS20P housing"], ["Tool changer (arm side)", "J6 XS20P housing"], ["Tool plate", "Tool changer (arm side)"],
             ["Tool plate", "V-groove dowels 4x12"], ["Coupling balls 8", "V-groove dowels 4x12"], ["Base housing", "J1 L30 motor"],
             ["J1 L30 cycloid disc A", "J1 L30 ring pins"], ["J2 L50 cycloid disc A", "J2 L50 ring pins"], ["J2 L50 cycloid disc B", "J2 L50 ring pins"],
             ["J2 L50 cycloid disc A", "J2 L50 output pins"], ["J5 XS20 cycloid disc B", "J5 XS20 output pins"], ["J3 M30 cam", "J3 M30 bearings"]] {
    let v = (find(parts, pair[0]) & find(parts, pair[1])).volume();
    checks.push(#{pair: pair[0] + " / " + pair[1], overlap_mm3: v});
}

// ================================================================ outputs
let placed = [];  let stretched = [];  let shapes_posed = [];
for p in parts {
    let s = posed(p.shape, p.group, pose, axes);
    shapes_posed.push(s);
    placed.push(#{shape: s, color: p.color});
    stretched.push(#{shape: p.shape, color: p.color});
}
let bom = [];
for p in parts {
    let file = p.name;
    for c in [" ", "(", ")", "/"] { file.replace(c, "-"); }
    file.replace("--", "-");
    if FILES && (p.kind == "printed" || p.kind == "cut") {
        export_step(p.shape, out + p.kind + "-" + file + ".step");
        if p.kind == "printed" { export_stl(p.shape, out + "print-" + file + ".stl", 0.05); }
    }
    bom.push(#{part: p.name, group: p.group, kind: p.kind, material: p.material, mass_g: if p.contains("mass") { p.mass } else { () },
               bbox: p.shape.bbox().size, valid: p.shape.is_valid()});
}
export_step(shapes_posed, out + "robot-arm-6dof-posed.step");
export_step(parts.map(|p| p.shape), out + "robot-arm-6dof-stretched.step");
render(placed, #{path: out + "robot-arm-6dof.png", width: 1600, height: 1200, view: "iso"});
render(stretched, #{path: out + "robot-arm-6dof-stretched.png", width: 1600, height: 900, view: "front"});
// one gearbox, opened up: housing at 35 % so the discs and pins show
let a2 = act["J2"];
render([#{shape: a2.housing, color: "#d9662a", opacity: 0.35}, #{shape: a2.disc1, color: C_D}, #{shape: a2.disc2, color: "#6b7178"},
        #{shape: a2.pins, color: C_ST}, #{shape: a2.opins, color: "#d6a83a"}, #{shape: a2.cam, color: "#555b62"}, #{shape: a2.carrier, color: "#b0b8c0", opacity: 0.5},
        #{shape: nema17(60.0), color: C_M}], #{path: out + "gearbox-L50.png", width: 1200, height: 1000, view: "iso"});
render([#{shape: tc_arm, color: C_D}, #{shape: compound(vpins), color: C_ST}, #{shape: compound(magnets), color: "#9aa3ab"}, #{shape: pogo, color: "#d6a83a"}],
       #{path: out + "tool-changer.png", width: 1000, height: 1000, eye: [xT + 120.0, yF + 60.0, H2 + 70.0], target: [xT, yF, H2], up: [0, 0, 1]});

let summary = #{
    reach_mm: reach, L1: L1, L2: L2, J2_height_mm: H2, wrist_to_interface_mm: xT - W,
    arm_mass_g: total, payload_g: PAYLOAD,
    moving_mass_g: #{J1: m1.mass, J2: m2.mass, J3: m3.mass, J5: m5.mass},
    torque: torque, interference: checks, parts: bom,
    joints: joints.map(|jn| #{joint: jn.j, gearbox: jn.spec.name, ratio: jn.ratio, motor: jn.spec.motor, bearings: jn.spec.bearings, ring_pins: jn.spec.pin,
                               od_mm: jn.spec.od, length_mm: act_z(jn.spec).out + jn.spec.motor_len}),
};
write_json(out + "summary.json", summary);
#{reach: reach, arm_mass_g: total, torque: torque, interference: checks.filter(|c| c.overlap_mm3 > 1.0)}
