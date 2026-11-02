// A micro FPV laser tag tank (about 160 mm long) built with the scripting API:
//   rcad script examples/fpv-tank/parts.rhai [output folder]
// Writes one STEP and one STL per printed part (in its print orientation), the assembly as STEP,
// renders, and parts.json with each part's mass and the build envelope checks.
// Units mm. World: X forward, Y left, Z up, ground at z = 0.
//
// Printed: hull tub, deck (PLA or ABS); turret shell, turret ring, barrel, sprockets, idlers and
// road wheels (PLA); tracks (TPU 95A).
// Bought (stand-ins with their outline sizes): 2 × N20 gear motors (6 V, 3 mm D shaft), SG90 servo
// (turret, ±90°), Seeed XIAO ESP32S3 Sense (camera, WiFi video, control), DRV8833 motor driver,
// MP1584 5 V buck, 2S 450 mAh LiPo, 940 nm 5 mm IR LED in the barrel, 4 × TSOP38238 hit sensors,
// M3 screws as wheel axles.

let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };

// ---------------------------------------------------------------- key dimensions
let BED = 254.0;             // printer build volume (cube)
let TRACK_Y0 = 35.5;         // inner edge of the left track
let TRACK_W = 18.0;          // track width
let BAND = 2.0;              // track band thickness
let PITCH = 8.0;             // lug pitch on the band's inner surface
let LUGS = 42;               // lugs per track: inner length 336 mm
let SPR_N = 10;              // sprocket pockets
let SPR_R = PITCH * SPR_N / (2.0 * PI());   // 12.73: sprocket drum radius
let SPR = [-62.0, 24.0];     // sprocket axle (x, z) = N20 motor shaft
let IDL = [67.4, 20.0];      // front idler axle (x, z), slot gives ±3 mm of tension
let IDL_R = 11.0;
let RW_R = 9.0;              // road wheel radius
let RW_X = [-38.0, -12.0, 14.0, 40.0];
let RW_Z = BAND + RW_R;      // 11: road wheel axle height
let WALL = 2.4;
let HULL_Y = 33.0;           // hull half width (outside)
let HULL_Z0 = 5.0;           // belly
let HULL_Z1 = 40.0;          // top of the tub = underside of the deck
let DECK_T = 2.4;
let TUR_Z0 = 43.2;           // underside of the turret ring (its bearing rib reaches down to 42.6)
let TUR_R = 32.0;
let CAM_Z = 66.0;            // camera axis height
let BARREL_Z = 54.5;         // IR barrel axis height

fn ycyl(x, y0, z, r, len) { cylinder(vec3(x, y0, z), vec3(0, 1, 0), r, len) }
fn zcyl(x, y, z0, r, h) { cylinder(vec3(x, y, z0), vec3(0, 0, 1), r, h) }
fn xcyl(x0, y, z, r, len) { cylinder(vec3(x0, y, z), vec3(1, 0, 0), r, len) }

// an arc-shaped slot in the XY plane: centre radius rc, width w, from angle a0 to a1 (degrees)
fn arc_slot(cx, cy, z0, rc, w, a0, a1, h) {
    let pts = [];
    let n = 24;
    for i in 0..=n {
        let a = (a0 + (a1 - a0) * i / n).to_radians();
        pts.push([cx + (rc + w / 2.0) * cos(a), cy + (rc + w / 2.0) * sin(a), z0]);
    }
    for i in 0..=n {
        let a = (a1 - (a1 - a0) * i / n).to_radians();
        pts.push([cx + (rc - w / 2.0) * cos(a), cy + (rc - w / 2.0) * sin(a), z0]);
    }
    let ends = [];
    for a in [a0, a1] {
        let r = a.to_radians();
        ends.push(zcyl(cx + rc * cos(r), cy + rc * sin(r), z0, w / 2.0, h));
    }
    fuse([extrude(polygon(pts), vec3(0, 0, h))] + ends)
}

// ---------------------------------------------------------------- bought parts (stand-ins)

// N20 gear motor, shaft along +Y: gearbox face at y = 0 centred on (0, 0), body towards -Y
fn n20() {
    let gear = block(vec3(-6, -9, -5), vec3(6, 0, 5));
    let motor = ycyl(0, -24, 0, 6, 15).common([block(vec3(-7, -25, -5), vec3(7, 0, 5))]);
    let shaft = ycyl(0, 0, 0, 1.5, 10).cut([block(vec3(1.25, -1, -2), vec3(2, 11, 2))]);
    fuse([gear, motor, ycyl(0, 0, 0, 2, 1), shaft, block(vec3(-2, -26, -1), vec3(2, -24, 1))])
}

// SG90 micro servo: output axis +Z, spline at the origin, top face z = 0, body along +X
fn sg90() {
    let body = block(vec3(-5.9, -6.1, -22.7), vec3(16.6, 6.1, 0));
    let tabs = block(vec3(-10.75, -6.1, -6.5), vec3(21.45, 6.1, -4)).cut([zcyl(-8.45, 0, -7, 1, 4), zcyl(19.15, 0, -7, 1, 4)]);
    fuse([body, tabs, zcyl(0, 0, 0, 2.4, 3.5), zcyl(0, 0, 2.0, 3.5, 2.0), block(vec3(-3, -16, 3.5), vec3(3, 16, 5))])
}

// XIAO ESP32S3 Sense stack standing up, camera looking along +X: lens tip at the origin
fn xiao_sense() {
    let stack = block(vec3(-19, -8.9, -10.5), vec3(-4, 8.9, 10.5));
    let cam = block(vec3(-4, -4, -4), vec3(-2, 4, 4));
    let lens = xcyl(-2, 0, 0, 3.5, 2);
    let usb = block(vec3(-18.5, -4.5, 10.5), vec3(-15.3, 4.5, 11.5));
    fuse([stack, cam, lens, usb])
}

// ---------------------------------------------------------------- drivetrain (left side; right is mirrored)

fn sprocket(r, n, pitch_w) {
    // drum on y 35.5..53.5 around the y axis through the origin, hub to y = 34, D bore for the N20
    let drum = fuse([ycyl(0, 35.5, 0, r, 18.0), ycyl(0, 34.0, 0, 5, 1.6)]);
    let pockets = [];
    for i in 0..n {
        pockets.push(block(vec3(-2.6, 41.0, r - 2.4), vec3(2.6, 48.0, r + 1)).rotate(vec3(0, 0, 0), vec3(0, 1, 0), 360.0 * i / n));
    }
    let bore = ycyl(0, 33.5, 0, 1.6, 9.5).cut([block(vec3(1.3, 33, -2), vec3(2, 44, 2))]);
    drum.cut(pockets + [bore])
}

// road wheel or idler on y 35.5..53.5 with a groove for the track lugs and a spacer hub to the hull
fn wheel(r) {
    let w = fuse([ycyl(0, 35.5, 0, r, 18.0), ycyl(0, 33.2, 0, 4.0, 2.4)]);
    w.cut([ycyl(0, 41.0, 0, r - 2.5, 7.0), ycyl(0, 30, 0, 1.7, 30)])
}

// the track as fitted, following the wheels: inner surface is the belt around them
fn track_fitted(circles, band, y0, w) {
    let pts = [];
    for c in circles {
        for i in 0..180 {
            let a = (2.0 * i).to_radians();
            pts.push([c[0] + c[2] * cos(a), c[1] + c[2] * sin(a)]);
        }
    }
    let inner = offset2d(polygon(convex_hull(pts)), 0.1)[0];   // 0.1 clearance over the chords
    let outer = offset2d(inner, band)[0];
    let b = extrude(outer, vec3(0, 0, w)).cut([extrude(inner, vec3(0, 0, w + 2.0)).translate(vec3(0, 0, -1))]);
    // XY → XZ plane (y becomes z), width along Y
    b.rotate_x(90.0).translate(vec3(0, y0 + w, 0))
}

fn stadium(s, rr, h) { fuse([block(vec3(-s / 2.0, -rr, 0), vec3(s / 2.0, rr, h)), zcyl(-s / 2.0, 0, 0, rr, h), zcyl(s / 2.0, 0, 0, rr, h)]).unify() }

// the track as printed: a TPU loop standing on the bed (stadium), lugs on the inside
fn track_print(lugs, pitch, band, w) {
    let len = lugs * pitch;
    let r = 25.0;
    let s = (len - 2.0 * PI() * r) / 2.0;
    let ring = stadium(s, r + band, w).cut([stadium(s, r, w + 2).translate(vec3(0, 0, -1))]);
    let tools = [];
    for i in 0..lugs {
        let t = pitch * i + pitch / 2.0;
        let p = [0.0, 0.0]; let ang = 0.0;
        if t < s { p = [-s / 2.0 + t, -r]; ang = 0.0; }
        else if t < s + PI() * r { let th = -PI() / 2.0 + (t - s) / r; p = [s / 2.0 + r * cos(th), r * sin(th)]; ang = th.to_degrees() + 90.0; }
        else if t < 2.0 * s + PI() * r { p = [s / 2.0 - (t - s - PI() * r), r]; ang = 180.0; }
        else { let th = PI() / 2.0 + (t - 2.0 * s - PI() * r) / r; p = [-s / 2.0 + r * cos(th), r * sin(th)]; ang = th.to_degrees() + 90.0; }
        tools.push(block(vec3(-2.0, -0.2, w / 2.0 - 3.0), vec3(2.0, 2.0, w / 2.0 + 3.0)).rotate_z(ang).translate(vec3(p[0], p[1], 0)));
    }
    fuse([ring] + tools)
}

// ---------------------------------------------------------------- hull tub
let tub = extrude(rounded_rect(vec3(1, 0, HULL_Z0), 144, 2.0 * HULL_Y, 5), vec3(0, 0, HULL_Z1 - HULL_Z0));
// sloped glacis at the front bottom
tub = tub.cut([extrude(polygon([[67, -40, HULL_Z0 - 1], [80, -40, HULL_Z0 - 1], [80, -40, 20]]), vec3(0, 80, 0))]);
let top_face = ();
for f in tub.faces() { if f.normal().z > 0.9 && f.center().z > HULL_Z1 - 0.1 { top_face = f; } }
tub = tub.shell([top_face], WALL);

let y_in = HULL_Y - WALL;    // inside face of the side walls
let adds = [];
let cuts = [];
for sy in [1.0, -1.0] {
    // road wheel axle bosses (M3 screws threaded into Ø2.5)
    for x in RW_X {
        adds.push(ycyl(x, sy * y_in - (if sy > 0.0 { 6.0 } else { 0.0 }), RW_Z, 4.5, 6.0));
        cuts.push(ycyl(x, -HULL_Y - 1, RW_Z, 1.25, 2.0 * HULL_Y + 2));
    }
    // idler tension slot (M3 screw and nut), pad behind it
    adds.push(block(vec3(61, if sy > 0.0 { y_in - 5 } else { -y_in }, 16.5), vec3(70.6, if sy > 0.0 { y_in } else { -y_in + 5 }, 26)));
    // motor: gearbox recessed 1.4 into the wall, shaft boss hole, two M1.6 screws, cradle below
    let yw = sy * (HULL_Y - 1.0);
    cuts.push(block(vec3(SPR[0] - 6.1, if sy > 0.0 { y_in - 1 } else { -HULL_Y + 1 }, SPR[1] - 5.1), vec3(SPR[0] + 6.1, if sy > 0.0 { HULL_Y - 1 } else { -y_in + 1 }, SPR[1] + 5.1)));
    cuts.push(ycyl(SPR[0], -HULL_Y - 1, SPR[1], 2.3, 2.0 * HULL_Y + 2));
    for dx in [-4.5, 4.5] { cuts.push(ycyl(SPR[0] + dx, -HULL_Y - 1, SPR[1], 0.8, 2.0 * HULL_Y + 2)); }
    adds.push(block(vec3(SPR[0] - 6, if sy > 0.0 { 9 } else { -y_in }, HULL_Z0 + WALL - 0.5), vec3(SPR[0] + 6, if sy > 0.0 { y_in } else { -9 }, SPR[1] - 5)));
    // battery strap slots in the floor
    cuts.push(block(vec3(34, sy * 17.0 - 1.5, HULL_Z0 - 1), vec3(46, sy * 17.0 + 1.5, HULL_Z0 + WALL + 1)));
}
// idler slots through the walls and pads
cuts.push(fuse([block(vec3(IDL[0] - 3, -HULL_Y - 1, IDL[1] - 1.7), vec3(IDL[0] + 3, HULL_Y + 1, IDL[1] + 1.7)),
    ycyl(IDL[0] - 3, -HULL_Y - 1, IDL[1], 1.7, 2.0 * HULL_Y + 2), ycyl(IDL[0] + 3, -HULL_Y - 1, IDL[1], 1.7, 2.0 * HULL_Y + 2)]));
// deck screw bosses
let deck_screws = [[-51.0, 27.5], [-51.0, -27.5], [57.0, 27.5], [57.0, -27.5]];
for p in deck_screws {
    adds.push(zcyl(p[0], p[1], HULL_Z0 + 1, 3.5, HULL_Z1 - HULL_Z0 - 1));
    cuts.push(zcyl(p[0], p[1], HULL_Z1 - 14, 1.25, 15));
}
// power switch in the rear wall (mini slide switch)
cuts.push(block(vec3(-74, -4.5, 31), vec3(-66, 4.5, 35)));
tub = fuse([tub] + adds).cut(cuts).unify();

// ---------------------------------------------------------------- deck (lid with fenders over the tracks)
let deck = extrude(rounded_rect(vec3(1, 0, HULL_Z1), 148, 114, 6), vec3(0, 0, DECK_T));
let d_adds = [];
// SG90 hangs under the deck, body across the hull towards +Y: tab holes at y = -8.45 and y = 19.15
for y in [-8.45, 19.15] {
    d_adds.push(block(vec3(-5, y - 2.4, HULL_Z1 - 5), vec3(5, y + 2.4, HULL_Z1)).cut([zcyl(0, y, HULL_Z1 - 6, 0.8, 7)]));
}
let d_cuts = [zcyl(0, 0, HULL_Z1 - 6, 6.0, 12), zcyl(22, 0, HULL_Z1 - 6, 4.5, 12)];   // spline and wire hole
for p in deck_screws { d_cuts.push(zcyl(p[0], p[1], HULL_Z1 - 6, 1.7, 12)); }
deck = fuse([deck] + d_adds).cut(d_cuts).unify();

// ---------------------------------------------------------------- turret
let ring_z1 = TUR_Z0 + 4.0;
let shell_z1 = ring_z1 + 34.0;
let tur_screws = [];
for a in [60.0, 180.0, 300.0] { tur_screws.push([28.3 * cos(a.to_radians()), 28.3 * sin(a.to_radians())]); }

// turret ring: screwed to the servo horn, carries the shell; the wire slot allows ±90° of rotation
let tring = fuse([zcyl(0, 0, TUR_Z0, TUR_R, ring_z1 - TUR_Z0),
    // bearing rib on the underside that rides on the deck
    zcyl(0, 0, TUR_Z0 - 0.6, TUR_R - 1.0, 0.6).cut([zcyl(0, 0, TUR_Z0 - 1, TUR_R - 3.0, 4)])]);
let horn = fuse([block(vec3(-17, -3.2, TUR_Z0 - 1), vec3(17, 3.2, TUR_Z0 + 1.6)), block(vec3(-3.2, -17, TUR_Z0 - 1), vec3(3.2, 17, TUR_Z0 + 1.6)), zcyl(0, 0, TUR_Z0 - 1, 4.0, 2.6)]);
let r_cuts = [horn, zcyl(0, 0, TUR_Z0 - 1, 1.2, 10), arc_slot(0, 0, TUR_Z0 - 1, 22.0, 8.0, -90.0, 90.0, 10)];
for p in tur_screws { r_cuts.push(zcyl(p[0], p[1], TUR_Z0 - 1, 1.1, 10)); }
for a in [45.0, 135.0, 225.0, 315.0] {
    // horn screw holes on the arms
    let r = a.to_radians();
    r_cuts.push(zcyl(12.0 * cos(r - PI() / 4.0), 12.0 * sin(r - PI() / 4.0), TUR_Z0 - 1, 0.9, 10));
}
tring = tring.cut(r_cuts).unify();

// turret shell: flat front with the camera window and the barrel socket, hit sensors in the walls
let tshell = zcyl(0, 0, ring_z1, TUR_R, shell_z1 - ring_z1).cut([block(vec3(28, -40, 0), vec3(40, 40, 100))]);
let top_e = [];
for e in tshell.edges() { if e.mid().z > shell_z1 - 0.1 && e.kind() == "circle" { top_e.push(e); } }
tshell = tshell.chamfer(top_e, 4.0);
let inner = zcyl(0, 0, ring_z1 - 1, TUR_R - 2.0, shell_z1 - ring_z1 - 1.0).cut([block(vec3(26, -40, 0), vec3(40, 40, 100))]);
let s_cuts = [inner,
    xcyl(20, 0, CAM_Z, 4.5, 20),            // camera window
    xcyl(20, 0, BARREL_Z, 5.7, 20),         // barrel socket
    zcyl(0, 0, shell_z1 - 5, 2.6, 10),      // hit LED (WS2812) in the roof
    block(vec3(7.5, -5, shell_z1 - 5), vec3(12.5, 5, shell_z1 + 1))];   // USB-C for programming
for a in [45.0, 135.0, 225.0, 315.0] {
    // TSOP38238 pockets: 6 wide, 7 tall through the wall
    s_cuts.push(block(vec3(TUR_R - 3, -3.0, 68.5), vec3(TUR_R + 1, 3.0, 75.5)).rotate_z(a));
}
let s_adds = [];
for p in tur_screws { s_adds.push(zcyl(p[0], p[1], ring_z1, 2.4, 8).cut([zcyl(p[0], p[1], ring_z1 - 1, 0.8, 8)])); }
// cradle for the XIAO stack: two rails either side of it
for sy in [1.0, -1.0] { s_adds.push(block(vec3(8, sy * 9.0 - (if sy > 0.0 { 0.0 } else { 2.0 }), ring_z1), vec3(23, sy * 9.0 + (if sy > 0.0 { 2.0 } else { 0.0 }), CAM_Z - 10.5))); }
tshell = fuse([tshell.cut(s_cuts)] + s_adds).unify();

// IR barrel: pushed into the socket, flange against the front face; LED seated at the back
let barrel = fuse([xcyl(25.0, 0, BARREL_Z, 5.5, 40.0), xcyl(28.0, 0, BARREL_Z, 7.0, 1.5)])
    .cut([xcyl(24, 0, BARREL_Z, 2.6, 45), xcyl(24, 0, BARREL_Z, 3.0, 2.2)]);

// ---------------------------------------------------------------- drivetrain placement
let sprocket_l = sprocket(SPR_R, SPR_N, 7.0);
let idler_l = wheel(IDL_R);
let road_l = wheel(RW_R);
let circles = [[SPR[0], SPR[1], SPR_R], [IDL[0], IDL[1], IDL_R]];
for x in RW_X { circles.push([x, RW_Z, RW_R]); }
let track_l = track_fitted(circles, BAND, TRACK_Y0, TRACK_W);

let o = vec3(0, 0, 0);
let ny = vec3(0, 1, 0);
let left = [#{name: "sprocket", shape: sprocket_l.translate(vec3(SPR[0], 0, SPR[1]))},
            #{name: "idler", shape: idler_l.translate(vec3(IDL[0], 0, IDL[1]))},
            #{name: "track", shape: track_l}];
for x in RW_X { left.push(#{name: "road wheel", shape: road_l.translate(vec3(x, 0, RW_Z))}); }

// ---------------------------------------------------------------- bought parts placed
let motor_l = n20().translate(vec3(SPR[0], HULL_Y - 1.0, SPR[1]));
let servo = sg90().rotate_z(90.0).translate(vec3(0, 0, HULL_Z1 - 1.0));
let xiao = xiao_sense().translate(vec3(27.0, 0, CAM_Z));
let battery = block(vec3(7.0, -15.5, HULL_Z0 + WALL), vec3(65.0, 15.5, HULL_Z0 + WALL + 14));
let drv = block(vec3(-52, 4, HULL_Z0 + WALL), vec3(-33.5, 20, HULL_Z0 + WALL + 1.6));
let buck = block(vec3(-52, -21, HULL_Z0 + WALL), vec3(-30, -4, HULL_Z0 + WALL + 4));
let led = xcyl(27.0, 0, BARREL_Z, 2.5, 8.6);

// ---------------------------------------------------------------- parts list, masses, files
let PLA = 1.24e-3; let TPU = 1.21e-3;
// printed mass ≈ solid mass × fill (walls + 20 % infill on parts this thin is about 60 %)
let printed = [
    #{name: "hull-tub", shape: tub, density: PLA, fill: 0.7, qty: 1, layout: tub},
    #{name: "deck", shape: deck, density: PLA, fill: 0.8, qty: 1, layout: deck.mirror(o, vec3(0, 0, 1))},
    #{name: "turret-shell", shape: tshell, density: PLA, fill: 0.7, qty: 1, layout: tshell.mirror(o, vec3(0, 0, 1))},
    #{name: "turret-ring", shape: tring, density: PLA, fill: 0.8, qty: 1, layout: tring.mirror(o, vec3(0, 0, 1))},
    #{name: "barrel", shape: barrel, density: PLA, fill: 1.0, qty: 1, layout: barrel.rotate(o, ny, -90.0)},
    #{name: "sprocket", shape: sprocket_l, density: PLA, fill: 0.8, qty: 2, layout: sprocket_l.rotate_x(-90.0)},
    #{name: "idler", shape: idler_l, density: PLA, fill: 0.8, qty: 2, layout: idler_l.rotate_x(-90.0)},
    #{name: "road-wheel", shape: road_l, density: PLA, fill: 0.8, qty: 8, layout: road_l.rotate_x(-90.0)},
    #{name: "track", shape: track_l, density: TPU, fill: 1.0, qty: 2, layout: track_print(LUGS, PITCH, BAND, TRACK_W)},
];
let bought = [
    #{name: "N20 gear motor 6 V", mass: 10.0, qty: 2}, #{name: "SG90 servo", mass: 9.0, qty: 1},
    #{name: "XIAO ESP32S3 Sense", mass: 7.0, qty: 1}, #{name: "2S 450 mAh LiPo", mass: 28.0, qty: 1},
    #{name: "DRV8833 board", mass: 2.0, qty: 1}, #{name: "MP1584 buck", mass: 2.0, qty: 1},
    #{name: "IR LED, TSOPs, WS2812, switch, wire", mass: 8.0, qty: 1}, #{name: "M3 screws and nuts", mass: 14.0, qty: 1},
];

let summary = #{parts: [], bought: bought, printed_g: 0.0, total_g: 0.0};
for p in printed {
    let b0 = p.layout.bbox();
    let lay = p.layout.translate(vec3(-b0.center.x, -b0.center.y, -b0.min.z));   // centred on the bed
    let bb = lay.bbox();
    let fits = bb.size.x <= BED && bb.size.y <= BED && bb.size.z <= BED && bb.min.z > -0.01;
    let g = mass_props(p.shape, p.density).mass * p.fill;
    summary.parts.push(#{name: p.name, qty: p.qty, grams_each: g, print_size: [bb.size.x, bb.size.y, bb.size.z], fits_bed: fits, valid: p.shape.is_valid()});
    summary.printed_g += g * p.qty;
    export_step(lay, out + p.name + ".step");
    export_stl(lay, out + p.name + ".stl", 0.05);
}
for b in bought { summary.total_g += b.mass * b.qty; }
summary.total_g += summary.printed_g;

// assembly (left side and its mirror image)
let asm = [#{name: "hull-tub", shape: tub, color: "#5b6b4a"}, #{name: "deck", shape: deck, color: "#6f805c"},
           #{name: "turret-ring", shape: tring, color: "#4f5d40"}, #{name: "turret-shell", shape: tshell, color: "#6f805c"}, #{name: "barrel", shape: barrel, color: "#2b2b2b"}];
let steps = [tub, deck, tring, tshell, barrel];
for side in [0, 1] {
    for p in left {
        let s = if side == 0 { p.shape } else { p.shape.mirror(o, ny) };
        asm.push(#{name: p.name + (if side == 0 { " L" } else { " R" }), shape: s, color: if p.name == "track" { "#262626" } else { "#9a9a8a" }});
        steps.push(s);
    }
    let m = if side == 0 { motor_l } else { motor_l.mirror(o, ny) };
    asm.push(#{name: if side == 0 { "motor L" } else { "motor R" }, shape: m, color: "#c8a040"});
}
for b in [[servo, "#2f6db5", "servo"], [xiao, "#1d7a46", "xiao"], [battery, "#b83a3a", "battery"], [drv, "#7a3fb0", "drv8833"], [buck, "#2f8fb5", "buck"], [led, "#dddddd", "ir-led"]] {
    asm.push(#{name: b[2], shape: b[0], color: b[1]});
}
// interference check between every pair of placed parts (bounding boxes first)
let clashes = [];
for i in 0..asm.len() {
    for j in (i + 1)..asm.len() {
        let a = asm[i].shape.bbox(); let b = asm[j].shape.bbox();
        if a.max.x < b.min.x || b.max.x < a.min.x || a.max.y < b.min.y || b.max.y < a.min.y || a.max.z < b.min.z || b.max.z < a.min.z { continue; }
        let v = (asm[i].shape & asm[j].shape).volume();
        if v > 0.5 { clashes.push(#{a: asm[i].name, b: asm[j].name, mm3: v}); }
    }
}
summary.clashes = clashes;
let bb_all = compound(steps).bbox();
summary.overall = [bb_all.size.x, bb_all.size.y, bb_all.size.z];
export_step(steps, out + "fpv-tank-assembly.step");
write_json(out + "parts.json", summary);
render(asm, #{path: out + "fpv-tank.png", width: 1400, height: 1000, view: "iso"});
render(asm, #{path: out + "fpv-tank-side.png", width: 1400, height: 700, view: "front"});
render(asm, #{path: out + "fpv-tank-end.png", width: 1000, height: 700, view: "right"});
let xray = [];
for a in asm { xray.push(#{shape: a.shape, color: a.color, opacity: if a.color == "#5b6b4a" || a.color == "#6f805c" || a.color == "#4f5d40" { 0.25 } else { 1.0 }}); }
render(xray, #{path: out + "fpv-tank-xray.png", width: 1400, height: 1000, view: "iso"});
summary
