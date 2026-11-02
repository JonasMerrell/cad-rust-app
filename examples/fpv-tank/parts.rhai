// A micro FPV laser tag tank (about 160 mm long) built with the scripting API:
//   rcad script examples/fpv-tank/parts.rhai [output folder]
// Writes one STEP and one STL per printed part (in its print orientation), the assembly as STEP,
// renders, and parts.json with each part's mass and the build envelope checks.
// Units mm. World: X forward, Y left, Z up, ground at z = 0.
//
// Printed: hull tub, deck (PLA or ABS); turret shell, turret ring, cupola, gun drum, barrel,
// sprockets, idlers and road wheels (PLA); tracks (TPU 95A).
// Bought (stand-ins with their outline sizes): 2 × N20 gear motors (6 V, 3 mm D shaft), 2 × SG90
// servos (turret ±90°, gun −6°…+20°), Radxa Zero 3W (Pi Zero size; WiFi, H.264 video) standing
// along the hull side, OV5647 Pi camera v1.3 in the gun drum so
// it elevates with the barrel, RP2040-Zero for the real-time IO, DRV8833 motor driver, MP1584 5 V buck, 2S 450 mAh LiPo, 940 nm 5 mm IR LED in the barrel, 4 × TSOP38238 hit sensors,
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
let TUR_R = 30.0;             // turret ring radius (the shell above it is faceted and wider)
let GUN = [24.0, 65.5];      // gun drum pivot (x, z): the camera looks out along this axis
let DRUM_R = 15.5;           // gun drum radius, half width 11
let CAM_Z = GUN[1] + 3.0;    // camera axis height (the camera board is mounted upside down)
let BARREL_Z = GUN[1] - 8.0; // IR barrel axis height (shows at the bottom of the video as an aim mark)
let ELEV = [-6.0, 20.0];     // gun depression and elevation limits (degrees)

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

// SG90 without its horn
fn sg90_body() {
    let body = block(vec3(-5.9, -6.1, -22.7), vec3(16.6, 6.1, 0));
    let tabs = block(vec3(-10.75, -6.1, -6.5), vec3(21.45, 6.1, -4)).cut([zcyl(-8.45, 0, -7, 1, 4), zcyl(19.15, 0, -7, 1, 4)]);
    fuse([body, tabs, zcyl(0, 0, 0, 2.4, 3.5), zcyl(0, 0, 2.0, 3.5, 2.0)])
}

// SG90 micro servo: output axis +Z, spline at the origin, top face z = 0, body along +X
fn sg90() {
    let body = block(vec3(-5.9, -6.1, -22.7), vec3(16.6, 6.1, 0));
    let tabs = block(vec3(-10.75, -6.1, -6.5), vec3(21.45, 6.1, -4)).cut([zcyl(-8.45, 0, -7, 1, 4), zcyl(19.15, 0, -7, 1, 4)]);
    fuse([body, tabs, zcyl(0, 0, 0, 2.4, 3.5), zcyl(0, 0, 2.0, 3.5, 2.0), block(vec3(-3, -16, 3.5), vec3(3, 16, 5))])
}

// OV5647 Pi camera (25 × 24 board), upside down, looking along +X: lens centre at the origin,
// board front face at x = -6.5 (so the lens tip is at the origin)
fn pi_cam() {
    let board = block(vec3(-7.5, -12.5, -14.5), vec3(-6.5, 12.5, 9.4));
    let cmod = block(vec3(-6.5, -4.25, -4.25), vec3(-2, 4.25, 4.25));
    fuse([board, cmod, xcyl(-2, 0, 0, 3.6, 2)])
}

// Radxa Zero 3W (Pi Zero size, 65 × 30) standing on its long edge, component side towards +Y: corner at the origin
fn pi_zero() {
    fuse([block(vec3(0, 0, 0), vec3(65, 1.4, 30)), block(vec3(10, 1.4, 8), vec3(50, 3.4, 22)), block(vec3(60, 1.4, 9), vec3(65, 2.6, 21))])
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
    rim_chamfer(drum.cut(pockets + [bore] + face_pockets(5, 7.6, 2.1)), r)
}

// road wheel or idler on y 35.5..53.5 with a groove for the track lugs and a spacer hub to the hull
fn wheel(r) {
    let w = fuse([ycyl(0, 35.5, 0, r, 18.0), ycyl(0, 33.2, 0, 4.0, 2.4)]);
    let n = if r > 10.0 { 6 } else { 5 };
    let cuts = [ycyl(0, 41.0, 0, r - 2.5, 7.0), ycyl(0, 30, 0, 1.7, 30)];
    cuts += face_pockets(n, r * 0.62, r * 0.19);
    rim_chamfer(w.cut(cuts), r)
}

// round pockets in the outer face (y = 53.5) of a wheel, 1.6 deep
fn face_pockets(n, pcd_r, r) {
    let ps = [];
    for i in 0..n {
        let a = (360.0 * i / n + 90.0).to_radians();
        ps.push(ycyl(pcd_r * cos(a), 51.9, pcd_r * sin(a), r, 3.0));
    }
    ps
}

// chamfer the outer rim edge of a wheel (the edge on the bed when printed)
fn rim_chamfer(w, r) {
    let es = [];
    for e in w.edges() {
        let c = e.circle();
        if c != () && (e.mid().y - 53.5).abs() < 0.01 && (c.radius - r).abs() < 0.01 { es.push(e); }
    }
    if es.len() > 0 { w.chamfer(es, 0.8) } else { w }
}

// the track as fitted, following the wheels: inner surface is the belt around them
fn track_fitted(circles, band, y0, w, with_grousers) {
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
    if !with_grousers { return b.rotate_x(90.0).translate(vec3(0, y0 + w, 0)); }
    // grousers on the outside every lug pitch, walking the belt outline
    let hp = convex_hull(pts);
    let ribs = [];
    let next = 4.0; let run = 0.0;
    for i in 0..hp.len() {
        let a = hp[i]; let c = hp[(i + 1) % hp.len()];
        let dx = c.x - a.x; let dz = c.y - a.y;
        let l = (dx * dx + dz * dz).sqrt();
        if l < 1e-9 { continue; }
        let tx = dx / l; let tz = dz / l; let nx = tz; let nz = -tx;   // outward normal (counter-clockwise outline)
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
    b = fuse([b] + ribs);
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
    for i in 0..lugs {
        // grousers on the outside, between the lugs
        let t = pitch * i;
        let p = [0.0, 0.0]; let ang = 0.0;
        if t < s { p = [-s / 2.0 + t, -r]; ang = 0.0; }
        else if t < s + PI() * r { let th = -PI() / 2.0 + (t - s) / r; p = [s / 2.0 + r * cos(th), r * sin(th)]; ang = th.to_degrees() + 90.0; }
        else if t < 2.0 * s + PI() * r { p = [s / 2.0 - (t - s - PI() * r), r]; ang = 180.0; }
        else { let th = PI() / 2.0 + (t - 2.0 * s - PI() * r) / r; p = [-s / 2.0 + r * cos(th), r * sin(th)]; ang = th.to_degrees() + 90.0; }
        tools.push(extrude(polygon([[-1.2, -band + 0.3, 0], [1.2, -band + 0.3, 0], [1.0, -band - 1.0, 0], [-1.0, -band - 1.0, 0]]), vec3(0, 0, w)).rotate_z(ang).translate(vec3(p[0], p[1], 0)));
    }
    fuse([ring] + tools)
}

// ---------------------------------------------------------------- hull tub
// side profile: lower and upper glacis at the front, sloped plate at the rear; rounded corners in plan
let profile = polygon([[-71, -40, HULL_Z0], [67, -40, HULL_Z0], [73, -40, 12], [73, -40, 28], [63, -40, HULL_Z1], [-64, -40, HULL_Z1], [-71, -40, 31]]);
let tub = extrude(profile, vec3(0, 80, 0)).common([extrude(rounded_rect(vec3(1, 0, HULL_Z0 - 1), 144, 2.0 * HULL_Y, 5), vec3(0, 0, 50))]);
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
// standoffs for the Radxa Zero 3W (Pi Zero holes: M2.5, 58 × 23) from the right wall
for x in [-41.5, 16.5] { for z in [13.0, 36.0] {
    adds.push(ycyl(x, -y_in - 0.5, z, 2.6, y_in + 0.5 - 23.5));
    cuts.push(ycyl(x, -y_in - 1, z, 1.05, 9));
} }
tub = fuse([tub] + adds).cut(cuts).unify();

// ---------------------------------------------------------------- deck (lid with fenders over the tracks)
// outline: the hull's glacis shows between the fenders, fender ends cut at 45°
let deck = extrude(polygon([[63, -33, HULL_Z1], [76, -33, HULL_Z1], [76, -51, HULL_Z1], [70, -57, HULL_Z1], [-70, -57, HULL_Z1], [-76, -51, HULL_Z1], [-76, -33, HULL_Z1],
    [-64, -33, HULL_Z1], [-64, 33, HULL_Z1], [-76, 33, HULL_Z1], [-76, 51, HULL_Z1], [-70, 57, HULL_Z1], [70, 57, HULL_Z1], [76, 51, HULL_Z1], [76, 33, HULL_Z1], [63, 33, HULL_Z1]]), vec3(0, 0, DECK_T));
let top_edges = [];
for e in deck.edges() { if (e.start().z - HULL_Z1 - DECK_T).abs() < 0.01 && (e.end().z - HULL_Z1 - DECK_T).abs() < 0.01 { top_edges.push(e); } }
deck = deck.chamfer(top_edges, 0.8);
let d_adds = [];
// side skirts over the top run of the tracks
for sy in [1.0, -1.0] {
    let y0 = if sy > 0.0 { 55.4 } else { -57.0 };
    d_adds.push(extrude(polygon([[-62, y0, HULL_Z1], [64, y0, HULL_Z1], [59, y0, 31], [-57, y0, 31]]), vec3(0, 1.6, 0)));
}
// SG90 hangs under the deck, body across the hull towards +Y: tab holes at y = -8.45 and y = 19.15
for y in [-8.45, 19.15] {
    d_adds.push(block(vec3(-5, y - 2.4, HULL_Z1 - 5), vec3(5, y + 2.4, HULL_Z1)).cut([zcyl(0, y, HULL_Z1 - 6, 0.8, 7)]));
}
let d_cuts = [zcyl(0, 0, HULL_Z1 - 6, 6.0, 12), block(vec3(19, -8, HULL_Z1 - 6), vec3(25, 8, HULL_Z1 + 6))];   // spline; slot for the camera ribbon and wires
for p in deck_screws {
    // countersunk M3
    d_cuts.push(zcyl(p[0], p[1], HULL_Z1 - 6, 1.7, 12));
    d_cuts.push(cone(vec3(p[0], p[1], HULL_Z1 + DECK_T - 1.6), vec3(0, 0, 1), 1.7, 3.3, 1.61));
}
// engraved engine grille behind the turret and a driver's hatch at the front (0.6 deep)
let zt = HULL_Z1 + DECK_T;
for y in [-16.0, -8.0, 0.0, 8.0, 16.0] { d_cuts.push(block(vec3(-62, y - 1.2, zt - 0.6), vec3(-44, y + 1.2, zt + 1))); }
d_cuts.push(zcyl(46, -14, zt - 0.6, 7.5, 2).cut([zcyl(46, -14, zt - 1, 6.5, 3)]));
d_cuts.push(zcyl(46, -14, zt - 0.6, 1.5, 2));
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

// turret shell: faceted, sloped walls lofted from the base outline to a smaller roof, a bustle at the
// rear; the gun drum sits in a round embrasure in the front
fn outline_at(pts, z) { let w = []; for p in pts { w.push([p[0], p[1], z]); } wire(w, true) }
let base_pts = [[30, -12], [30, 12], [18, 32], [-24, 32], [-40, 18], [-40, -18], [-24, -32], [18, -32]];
let roof_pts = [[25, -10], [25, 10], [15, 27], [-22, 27], [-36, 15], [-36, -15], [-22, -27], [15, -27]];
let tvol = loft([outline_at(base_pts, ring_z1), outline_at(roof_pts, shell_z1)], true);
let bottom = ();
for f in tvol.faces() { if f.normal().z < -0.9 { bottom = f; } }
let tshell = tvol.shell([bottom], 2.0);
let roof_e = [];
for e in tshell.edges() { if (e.start().z - shell_z1).abs() < 0.01 && (e.end().z - shell_z1).abs() < 0.01 { roof_e.push(e); } }
tshell = tshell.chamfer(roof_e, 1.2);
// the cupola is a separate part (so the shell can print roof down): a spigot goes into the roof
let cupola = zcyl(-14, 12, shell_z1, 8, 3);
let ce = [];
for e in cupola.edges() { if e.mid().z > shell_z1 + 2.9 { ce.push(e); } }
cupola = fuse([cupola.chamfer(ce, 1.0), zcyl(-14, 12, shell_z1 - 2, 3.2, 2.1)])
    .cut([zcyl(-14, 12, shell_z1 - 3, 2.0, 8), zcyl(-14, 12, shell_z1 + 2.4, 6.0, 2)]);   // light pipe hole for the hit LED, hatch recess
let s_cuts = [
    ycyl(GUN[0], -14.7, GUN[1], DRUM_R + 0.3, 29.4),   // embrasure the gun drum turns in
    zcyl(-14, 12, shell_z1 - 5, 3.3, 12)];             // cupola spigot; the hit LED (WS2812) sits under it
for a in [45.0, 135.0, 225.0, 315.0] {
    // TSOP38238 windows: 6 wide, 7 tall through the wall
    s_cuts.push(block(vec3(22, -3.0, 68.5), vec3(50, 3.0, 75.5)).rotate_z(a));
}
let s_adds = [];
for p in tur_screws { s_adds.push(zcyl(p[0], p[1], ring_z1, 2.4, 8).cut([zcyl(p[0], p[1], ring_z1 - 1, 0.8, 8)])); }
// trunnion brackets either side of the drum (M2 pivot screws), hanging from the roof
for y0 in [14.9, -16.9] {
    s_adds.push(block(vec3(12, y0, 52), vec3(32, y0 + 2.0, shell_z1)).common([tvol]).cut([ycyl(GUN[0], -15, GUN[1], 1.1, 30)]));
}
// elevation servo (SG90) in the bustle, output axis along +Y at (-10, 65.5): two brackets for its tabs
for x in [-29.15, -1.55] {
    s_adds.push(block(vec3(x - 2.4, -3.5, 59.0), vec3(x + 2.4, 1.0, shell_z1)).common([tvol]).cut([ycyl(x, -4.5, GUN[1], 0.8, 6)]));
}
tshell = fuse([tshell.cut(s_cuts)] + s_adds).unify();

// gun drum: holds the Pi camera (looking out of the front window) and the barrel, turns on two M2
// screws in the trunnion brackets; open at the back to put the camera in and let its ribbon out.
// The camera board screws (M2) to four standoffs on a plate inside the front of the drum.
// A pushrod from the elevation servo's horn goes through the hole at 13 mm behind the pivot.
let gx = GUN[0]; let gz = GUN[1];
let DW = 14.4;   // drum half width
let inside = ycyl(gx, -DW + 1.6, gz, DRUM_R - 1.6, 2.0 * DW - 3.2);
let drum = ycyl(gx, -DW, gz, DRUM_R, 2.0 * DW).cut([inside, block(vec3(0, -DW + 1.6, 40), vec3(gx - 4.5, DW - 1.6, 90))]);
let cam_face = gx + 5.5;   // front face of the camera board
let plate = block(vec3(cam_face + 2.0, -DW + 1.6, gz - 14), vec3(cam_face + 4.0, DW - 1.6, gz + 13)).common([inside])
    .cut([block(vec3(cam_face, -5.2, CAM_Z - 5.2), vec3(cam_face + 5, 5.2, CAM_Z + 5.2))]);
let standoffs = [];
for y in [-10.5, 10.5] { for z in [CAM_Z, CAM_Z - 12.5] { standoffs.push(xcyl(cam_face, y, z, 2.0, 2.2).cut([xcyl(cam_face - 1, y, z, 0.8, 6)])); } }
let collar = xcyl(gx + 12.0, 0, BARREL_Z, 6.8, 3.5);
drum = fuse([drum, plate, collar] + standoffs).cut([
    xcyl(gx + 6, 0, CAM_Z, 4.5, 15),               // camera window
    xcyl(gx + 11.0, 0, BARREL_Z, 5.15, 7),         // barrel socket, 4.5 deep
    xcyl(gx, 0, BARREL_Z, 1.5, 14),                // LED leads
    ycyl(gx, -DW - 1, gz, 0.8, 2.0 * DW + 2),      // M2 pivot screws
    ycyl(gx - 13, DW - 3, gz, 0.8, 4)]).unify();   // pushrod

// IR barrel: pushed into the drum's socket, flange against the collar; LED seated at the back
// 45° cones on every step so it prints standing on its back end without supports
fn xcone(x0, z, r0, r1, len) { cone(vec3(x0, 0, z), vec3(1, 0, 0), r0, r1, len) }
let xr = gx + 11.0;   // back end of the barrel
let barrel = fuse([xcyl(xr, 0, BARREL_Z, 5.0, 43.0),
    xcyl(xr + 4.5, 0, BARREL_Z, 6.5, 1.0), xcone(xr + 5.5, BARREL_Z, 6.5, 5.0, 1.5),     // flange against the collar
    xcone(xr + 20.0, BARREL_Z, 5.0, 6.2, 1.2), xcyl(xr + 21.2, 0, BARREL_Z, 6.2, 6.0), xcone(xr + 27.2, BARREL_Z, 6.2, 5.0, 1.2),   // fume extractor
    xcone(xr + 35.8, BARREL_Z, 5.0, 6.0, 1.0), xcyl(xr + 36.8, 0, BARREL_Z, 6.0, 6.2)])  // muzzle
    .cut([xcyl(xr - 1, 0, BARREL_Z, 2.6, 46), xcyl(xr - 1, 0, BARREL_Z, 3.0, 4.2)]);

// ---------------------------------------------------------------- drivetrain placement
let sprocket_l = sprocket(SPR_R, SPR_N, 7.0);
let idler_l = wheel(IDL_R);
let road_l = wheel(RW_R);
let circles = [[SPR[0], SPR[1], SPR_R], [IDL[0], IDL[1], IDL_R]];
for x in RW_X { circles.push([x, RW_Z, RW_R]); }
let track_l = track_fitted(circles, BAND, TRACK_Y0, TRACK_W, true);

let o = vec3(0, 0, 0);
let ny = vec3(0, 1, 0);
let left = [#{name: "sprocket", shape: sprocket_l.translate(vec3(SPR[0], 0, SPR[1]))},
            #{name: "idler", shape: idler_l.translate(vec3(IDL[0], 0, IDL[1]))},
            #{name: "track", shape: track_l}];
for x in RW_X { left.push(#{name: "road wheel", shape: road_l.translate(vec3(x, 0, RW_Z))}); }

// ---------------------------------------------------------------- bought parts placed
let motor_l = n20().translate(vec3(SPR[0], HULL_Y - 1.0, SPR[1]));
let servo = sg90().rotate_z(90.0).translate(vec3(0, 0, HULL_Z1 - 1.0));
let cam = pi_cam().translate(vec3(cam_face + 6.5, 0, CAM_Z));
let elev_servo = sg90_body().rotate_z(180.0).rotate_x(-90.0).translate(vec3(-10, 7.5, GUN[1]));
let elev_horn = block(vec3(-10, 11.5, GUN[1] - 1.5), vec3(3, 13.0, GUN[1] + 1.5));
// Radxa Zero 3W standing along the right hull wall on four M2.5 standoffs, RP2040-Zero on the floor
let pi = pi_zero().rotate_z(180.0).translate(vec3(20, -18.5, 9.5));
let rp2040 = block(vec3(-27, 0, HULL_Z0 + WALL), vec3(-3.5, 18, HULL_Z0 + WALL + 4));
let battery = block(vec3(7.0, -15.5, HULL_Z0 + WALL), vec3(65.0, 15.5, HULL_Z0 + WALL + 14));
let drv = block(vec3(-52, 4, HULL_Z0 + WALL), vec3(-33.5, 20, HULL_Z0 + WALL + 1.6));
let buck = block(vec3(-52, -17, HULL_Z0 + WALL), vec3(-30, -1, HULL_Z0 + WALL + 4));
let led = xcyl(xr, 0, BARREL_Z, 2.5, 8.6);

// ---------------------------------------------------------------- animation (third argument "animate")
if ARGS.len() > 1 && ARGS[1] == "animate" {
    import "anim" as anim;
    let belt = [];
    for c in circles { for i in 0..180 { let a = (2.0 * i).to_radians(); belt.push([c[0] + c[2] * cos(a), c[1] + c[2] * sin(a)]); } }
    let wheels = [#{shape: sprocket_l.translate(vec3(SPR[0], 0, SPR[1])), x: SPR[0], z: SPR[1], r: SPR_R},
                  #{shape: idler_l.translate(vec3(IDL[0], 0, IDL[1])), x: IDL[0], z: IDL[1], r: IDL_R}];
    for x in RW_X { wheels.push(#{shape: road_l.translate(vec3(x, 0, RW_Z)), x: x, z: RW_Z, r: RW_R}); }
    let t = #{
        hull: [#{shape: tub, paint: true}, #{shape: deck, paint: true},
               #{shape: motor_l, paint: false, color: "#c8a040"}, #{shape: motor_l.mirror(o, ny), paint: false, color: "#c8a040"}],
        wheels: wheels, band: track_fitted(circles, BAND, TRACK_Y0, TRACK_W, false), band_t: BAND,
        hull_pts: convex_hull(belt), track_y0: TRACK_Y0, track_w: TRACK_W,
        turret: [#{shape: tring, paint: false, color: "#454f33"}, #{shape: tshell, paint: true}],
        gun: [#{name: "gun-drum", shape: drum, paint: true}, #{name: "barrel", shape: barrel, paint: false, color: "#2a2a2a"},
              #{name: "pi camera", shape: cam, paint: false, color: "#1d7a46"}, #{name: "ir-led", shape: led, paint: false, color: "#dddddd"}],
        cupola: cupola, gx: gx, gz: gz, cam_z: CAM_Z, barrel_z: BARREL_Z, muzzle_x: xr + 43.0,
        max_frames: if ARGS.len() > 2 { parse_int(ARGS[2]) } else { 100000 },
    };
    return anim::run(t, out);
}

// ---------------------------------------------------------------- parts list, masses, files
let PLA = 1.24e-3; let TPU = 1.21e-3;
// printed mass ≈ solid mass × fill (walls + 20 % infill on parts this thin is about 60 %)
let printed = [
    #{name: "hull-tub", shape: tub, density: PLA, fill: 0.7, qty: 1, layout: tub},
    #{name: "deck", shape: deck, density: PLA, fill: 0.8, qty: 1, layout: deck.mirror(o, vec3(0, 0, 1))},
    #{name: "turret-shell", shape: tshell, density: PLA, fill: 0.7, qty: 1, layout: tshell.mirror(o, vec3(0, 0, 1))},
    #{name: "turret-ring", shape: tring, density: PLA, fill: 0.8, qty: 1, layout: tring.mirror(o, vec3(0, 0, 1))},
    #{name: "cupola", shape: cupola, density: PLA, fill: 1.0, qty: 1, layout: cupola.mirror(o, vec3(0, 0, 1))},
    #{name: "gun-drum", shape: drum, density: PLA, fill: 0.9, qty: 1, layout: drum.rotate_x(90.0)},
    #{name: "barrel", shape: barrel, density: PLA, fill: 1.0, qty: 1, layout: barrel.rotate(o, ny, -90.0)},
    #{name: "sprocket", shape: sprocket_l, density: PLA, fill: 0.8, qty: 2, layout: sprocket_l.rotate_x(-90.0)},
    #{name: "idler", shape: idler_l, density: PLA, fill: 0.8, qty: 2, layout: idler_l.rotate_x(-90.0)},
    #{name: "road-wheel", shape: road_l, density: PLA, fill: 0.8, qty: 8, layout: road_l.rotate_x(-90.0)},
    #{name: "track", shape: track_l, density: TPU, fill: 1.0, qty: 2, layout: track_print(LUGS, PITCH, BAND, TRACK_W)},
];
let bought = [
    #{name: "N20 gear motor 6 V", mass: 10.0, qty: 2}, #{name: "SG90 servo (turret and gun)", mass: 9.0, qty: 2},
    #{name: "Radxa Zero 3W + microSD + antenna", mass: 12.0, qty: 1}, #{name: "OV5647 camera + Zero ribbon", mass: 4.0, qty: 1}, #{name: "RP2040-Zero", mass: 2.0, qty: 1}, #{name: "2S 450 mAh LiPo", mass: 28.0, qty: 1},
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
let HULL_C = "#4e5b38"; let DECK_C = "#5f6e45"; let RING_C = "#454f33";
let hero = #{eye: [300, -250, 210], target: [0, 0, 38], up: [0, 0, 1], fov: 28};
let asm = [#{name: "hull-tub", shape: tub, color: HULL_C}, #{name: "deck", shape: deck, color: DECK_C},
           #{name: "turret-ring", shape: tring, color: RING_C}, #{name: "turret-shell", shape: tshell, color: DECK_C}, #{name: "cupola", shape: cupola, color: DECK_C}];
let steps = [tub, deck, tring, tshell, cupola];
for side in [0, 1] {
    for p in left {
        let s = if side == 0 { p.shape } else { p.shape.mirror(o, ny) };
        asm.push(#{name: p.name + (if side == 0 { " L" } else { " R" }), shape: s, color: if p.name == "track" { "#1e1e1e" } else { "#454a3e" }});
        steps.push(s);
    }
    let m = if side == 0 { motor_l } else { motor_l.mirror(o, ny) };
    asm.push(#{name: if side == 0 { "motor L" } else { "motor R" }, shape: m, color: "#c8a040"});
}
for b in [[servo, "#2f6db5", "servo"], [elev_servo, "#2f6db5", "elevation servo"], [elev_horn, "#dddddd", "elevation horn"],
          [battery, "#b83a3a", "battery"], [drv, "#7a3fb0", "drv8833"], [buck, "#2f8fb5", "buck"],
          [pi, "#1d7a46", "radxa zero 3w"], [rp2040, "#1d4f7a", "rp2040-zero"]] {
    asm.push(#{name: b[2], shape: b[0], color: b[1]});
}
// the gun: everything that elevates with the drum
let gun = [#{name: "gun-drum", shape: drum, color: DECK_C}, #{name: "barrel", shape: barrel, color: "#2a2a2a"},
           #{name: "pi camera", shape: cam, color: "#1d7a46"}, #{name: "ir-led", shape: led, color: "#dddddd"}];
fn posed(gun, gx, gz, deg) {
    let out = [];
    for g in gun { out.push(#{name: g.name, shape: g.shape.rotate(vec3(gx, 0, gz), vec3(0, 1, 0), -deg), color: g.color}); }
    out
}
// clashes of the gun with everything else at full depression and full elevation
let gun_clashes = [];
for deg in [ELEV[0], 0.0, ELEV[1]] {
    for g in posed(gun, gx, gz, deg) {
        for a in asm {
            let p = g.shape.bbox(); let q = a.shape.bbox();
            if p.max.x < q.min.x || q.max.x < p.min.x || p.max.y < q.min.y || q.max.y < p.min.y || p.max.z < q.min.z || q.max.z < p.min.z { continue; }
            let v = (g.shape & a.shape).volume();
            if v > 0.5 { gun_clashes.push(#{elevation: deg, a: g.name, b: a.name, mm3: v}); }
        }
    }
}
// full depression with the turret turned 45° and 90°: the barrel must clear the deck
for psi in [45.0, 90.0, -45.0, -90.0] {
    for g in posed(gun, gx, gz, ELEV[0]) {
        let gs = g.shape.rotate_z(psi);
        for a in asm {
            if a.name != "deck" && a.name != "hull-tub" { continue; }
            let v = (gs & a.shape).volume();
            if v > 0.5 { gun_clashes.push(#{elevation: ELEV[0], turret: psi, a: g.name, b: a.name, mm3: v}); }
        }
    }
}
summary.gun_clashes = gun_clashes;
for g in gun { asm.push(g); steps.push(g.shape); }
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
render(asm, #{path: out + "fpv-tank.png", width: 1400, height: 1000, eye: hero.eye, target: hero.target, up: hero.up, fov: hero.fov, background: "#eef0ea"});
render(asm, #{path: out + "fpv-tank-front.png", width: 1400, height: 1000, eye: [320, 180, 140], target: [0, 0, 35], up: [0, 0, 1], fov: 28, background: "#eef0ea"});
render(asm, #{path: out + "fpv-tank-side.png", width: 1400, height: 700, view: "front"});
render(asm, #{path: out + "fpv-tank-end.png", width: 1000, height: 700, view: "right"});
let raised = [];
for a in asm { if !gun.some(|g| g.name == a.name) { raised.push(a); } }
raised += posed(gun, gx, gz, 15.0);
render(raised, #{path: out + "fpv-tank-elevated.png", width: 1400, height: 1000, eye: hero.eye, target: hero.target, up: hero.up, fov: hero.fov, background: "#eef0ea"});
render(raised, #{path: out + "fpv-tank-elevated-side.png", width: 1400, height: 700, view: "front"});
let xray = [];
for a in asm { xray.push(#{shape: a.shape, color: a.color, opacity: if a.color == HULL_C || a.color == DECK_C || a.color == RING_C { 0.25 } else { 1.0 }}); }
render(xray, #{path: out + "fpv-tank-xray.png", width: 1400, height: 1000, eye: hero.eye, target: hero.target, up: hero.up, fov: hero.fov, background: "#eef0ea"});
summary
