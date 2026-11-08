// The FPV tank as an rCAD project built from sketches and features, so every part opens editable:
//   rcad script examples/fpv-tank/project.rhai [output folder]
// Writes "FPV Tank.json" (one Part Studio per printed part and an assembly with mates) and a STEP per
// part for checking it against parts.rhai (check_project.rhai). make_project.py zips it for import.
// Same world as parts.rhai: X forward, Y left, Z up, mm. The wheels, sprocket, barrel and cupola are
// modelled round their own axis and placed in the assembly; the hull, deck, turret and gun drum sit
// where they are in the tank.
import "feat" as f;

let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };
let only = if ARGS.len() > 1 { ARGS[1] } else { "" };   // build one Part Studio (by name) while working on it

// ---------------------------------------------------------------- key dimensions (as in parts.rhai)
let TRACK_Y0 = 35.5; let TRACK_W = 18.0; let BAND = 2.0; let PITCH = 8.0; let LUGS = 42; let SPR_N = 10;
let SPR_R = PITCH * SPR_N / (2.0 * PI()) - 1.1;
let SPR = [-62.0, 24.0]; let IDL = [65.4, 20.0]; let IDL_R = 11.0; let RW_R = 9.0; let RW_X = [-38.0, -12.0, 14.0, 40.0];
let RW_Z = BAND + RW_R; let WALL = 2.4;
let HULL_Y = 33.0; let HULL_Z0 = 5.0; let HULL_Z1 = 40.0; let DECK_T = 2.4;
let TUR_Z0 = 43.4; let TUR_R = 30.0; let GUN = [24.0, 65.5]; let DRUM_R = 15.5;
let CAM_Z = GUN[1] + 3.0; let BARREL_Z = GUN[1] - 8.0; let ELEV = [-6.0, 20.0];
let ring_z1 = TUR_Z0 + 4.0; let shell_z1 = ring_z1 + 34.0;
let gx = GUN[0]; let gz = GUN[1]; let DW = 14.4;
let xr = gx + 11.0;   // back end of the barrel

let d = new_document();
let studios = [];   // #{name, studio, color, material}
// start the next Part Studio (called as studios.begin(…); the first one is the document's own)
fn begin(d, name, color, material) {
    if this.len() > 0 { d.tool("new_part_studio", #{name: name}); }
    this.push(#{name: name, studio: this.len(), color: color, material: material});
}
// name and colour the Part Studio's parts
fn finish(d, names, out) {
    let s = this[this.len() - 1];
    let ps = d.parts();
    let list = [];
    for i in 0..ps.len() {
        let e = #{body: ps[i].body, name: if i < names.len() { names[i] } else { s.name }, color: s.color};
        if s.material != "" { e.material = s.material; }   // rCAD has no TPU yet
        list.push(e);
    }
    d.tool("set_parts", #{parts: list});
    let v = [];
    for p in d.parts() { v.push(#{name: p.name, volume: p.volume_mm3, body: p.body, centroid: p.centroid_mm}); }
    s.parts = v;
    d.export_file(out + s.name + ".step");
    this[this.len() - 1] = s;
}
fn want(only, name) { only == "" || only == name }

// ---------------------------------------------------------------- road wheel and idler
// on y 35.5..53.5 round the Y axis: a revolved section (groove for the track lugs with 45° sides, rim
// chamfer, Ø4.4 bore for the 4 mm brass sleeve), then lightening pockets in the outer face
fn wheel(d, r, n) {
    d.variable("r", r);
    d.variable("pockets", n);
    d.variable("pocket_d", r * 0.38);
    d.variable("pocket_pcd", r * 1.24);
    let sk = d.sketch("top", "Section");
    sk.polygon([[2.2, 33.2], [4.0, 33.2], [4.0, 35.5], [r, 35.5], [r, 38], [r - 3, 41], [r - 3, 48], [r, 51], [r, 52.7], [r - 0.8, 53.5], [2.2, 53.5]]);
    f::rev(d, sk, "y", "new", "Wheel");
    let p = f::sketch_at(d, "y", 53.5, "Pocket");
    p.circle([0, r * 0.62], "pocket_d");
    let cut = f::ext(d, p, "y", 53.5, 51.9, "remove", "Pocket");
    f::cpattern(d, [cut], "y", "pockets", "Pockets");
}

if want(only, "Road wheel") {
    studios.begin(d, "Road wheel", [69, 74, 62], "PLA");
    wheel(d, RW_R, 5);
    studios.finish(d, ["road wheel"], out);
}
if want(only, "Idler") {
    studios.begin(d, "Idler", [69, 74, 62], "PLA");
    wheel(d, IDL_R, 6);
    studios.finish(d, ["idler"], out);
}

// ---------------------------------------------------------------- sprocket
// drum on y 35.5..53.5, hub to y = 34 (clears the motor's M1.6 screw heads), D bore for the N20 shaft,
// 10 pockets for the track lugs with 45° sides
if want(only, "Sprocket") {
    studios.begin(d, "Sprocket", [69, 74, 62], "PLA");
    let r = SPR_R;
    d.variable("pockets", SPR_N);
    d.variable("pocket_depth", 2.9);
    let sk = d.sketch("top", "Section");
    // blind Ø3.2 bore for the N20 shaft from y = 33.5 to 43. (Drawn as a plain polyline: rCAD fails to
    // revolve a dimensioned polygon that has an edge on the axis.)
    sk.polyline([[0, 43], [1.6, 43], [1.6, 34], [2.8, 34], [2.8, 35.5], [r, 35.5], [r, 52.7], [r - 0.8, 53.5], [0, 53.5]], true);
    f::rev(d, sk, "y", "new", "Drum");
    // N20 D shaft: the flat 1.1 from the centre (snug; glued)
    let b = f::sketch_at(d, "y", 34.0, "D flat");
    let h = (1.6 * 1.6 - 1.1 * 1.1).sqrt();
    b.arc([0, 0], [1.1, -h], [1.1, h]);
    b.line([1.1, h], [1.1, -h]);
    f::ext(d, b, "y", 34.0, 43.0, "add", "D flat");
    // track lug pocket at the top, 5.2 wide (sketched on the YZ plane, extruded both ways)
    let p = d.sketch("right", "Lug pocket");
    let dp = 2.9;
    p.polygon([[41.0, r - dp], [48.0, r - dp], [48.0 + dp + 1.0, r + 1.0], [41.0 - dp - 1.0, r + 1.0]]);
    let cut = f::ext_sym(d, p, 5.2, "remove", "Lug pocket");
    f::cpattern(d, [cut], "y", "pockets", "Lug pockets");
    let q = f::sketch_at(d, "y", 53.5, "Face pocket");
    q.circle([0, 7.6], 4.2);
    let fc = f::ext(d, q, "y", 53.5, 51.9, "remove", "Face pocket");
    f::cpattern(d, [fc], "y", 5, "Face pockets");
    studios.finish(d, ["sprocket"], out);
}

// ---------------------------------------------------------------- barrel
// round the X axis from its back end (x = 0, at x = 35 in the tank): 45° cones on every step so it
// prints standing on its back end; bore for the 5 mm IR LED with a counterbore for its flange
if want(only, "Barrel") {
    studios.begin(d, "Barrel", [42, 42, 42], "PLA");
    d.variable("led_bore", 5.3);
    let sk = d.sketch("top", "Section");
    sk.polygon([[0, 3.1], [0, 5], [3.5, 5], [4.5, 6], [5.5, 6], [6.5, 5], [20, 5], [21.2, 6.2], [27.2, 6.2], [28.4, 5],
                [35.8, 5], [36.8, 6], [43, 6], [43, 2.65], [3.65, 2.65], [3.2, 3.1]]);
    f::rev(d, sk, "x", "new", "Barrel");
    studios.finish(d, ["barrel"], out);
}

// ---------------------------------------------------------------- cupola
// round the Z axis, its underside at z = 0 (on the turret roof): spigot into the roof, light pipe hole
// for the hit LED, engraved hatch ring
if want(only, "Cupola") {
    studios.begin(d, "Cupola", [95, 110, 69], "PLA");
    let sk = d.sketch("front", "Section");
    sk.polygon([[2, -2], [3.1, -2], [3.1, 0], [8, 0], [8, 2], [7, 3], [6, 3], [6, 2.4], [5, 2.4], [5, 3], [2, 3]]);
    f::rev(d, sk, "z", "new", "Cupola");
    studios.finish(d, ["cupola"], out);
}

// ---------------------------------------------------------------- turret ring
// screwed to the turret servo's horn, carries the shell; a bearing rib underneath rides 0.4 over the
// deck; the wire slot (±110°) lets the camera ribbon through at ±90°
let tur_screws = [];
for p in [[12.0, 30.0], [12.0, -30.0], [26.0, 135.0], [26.0, -135.0]] { tur_screws.push([p[0] * cos(p[1].to_radians()), p[0] * sin(p[1].to_radians())]); }
fn polar(r, a) { [r * cos(a.to_radians()), r * sin(a.to_radians())] }
if want(only, "Turret ring") {
    studios.begin(d, "Turret ring", [69, 79, 51], "PLA");
    d.variable("ring_r", TUR_R);
    d.variable("ring_t", 4);
    d.variable("horn_clear", 0.3);
    let base = f::plane(d, "z", TUR_Z0);
    let s = d.sketch(base, "Ring");
    s.circle([0, 0], "2 * ring_r");
    f::ext_expr(d, s, "z", "ring_t", true, "new", "Ring");
    let s = d.sketch(base, "Bearing rib");
    s.circle([0, 0], "2 * ring_r - 2");
    s.circle([0, 0], "2 * ring_r - 6");
    f::ext(d, [f::region(s, TUR_R - 2.0, 0.0)], "z", TUR_Z0, TUR_Z0 - 0.6, "add", "Bearing rib");
    // SG90 cross horn + 0.3 all round, 1.6 deep
    let s = f::sketch_at(d, "z", TUR_Z0 + 1.6, "Horn recess");
    s.rect([0, 0], "34 + 2 * horn_clear", "6.2 + 2 * horn_clear");
    s.rect([0, 0], "6.2 + 2 * horn_clear", "34 + 2 * horn_clear");
    s.circle([0, 0], "7.8 + 2 * horn_clear");
    f::ext(d, s, "z", TUR_Z0 + 1.6, TUR_Z0 - 1.0, "remove", "Horn recess");
    // centre hole (the horn's screw goes in through it), M2 pilots for the shell screws, horn screw
    // holes on the arms, and the wire slot
    let s = d.sketch(base, "Holes");
    s.circle([0, 0], 5.2);
    for p in tur_screws { s.circle(p, 1.6); }
    for a in [0.0, 90.0, 180.0, 270.0] { s.circle(polar(12.0, a), 1.8); }
    s.arc([0, 0], polar(26.0, -110.0), polar(26.0, 110.0));
    s.arc(polar(22.0, 110.0), polar(26.0, 110.0), polar(18.0, 110.0));
    s.arc([0, 0], polar(18.0, -110.0), polar(18.0, 110.0));
    s.arc(polar(22.0, -110.0), polar(18.0, -110.0), polar(26.0, -110.0));
    f::ext_sym(d, s, 20, "remove", "Holes and wire slot");
    studios.finish(d, ["turret ring"], out);
}

// ---------------------------------------------------------------- deck
// the lid on the hull tub with fenders over the tracks; the turret servo hangs under it
let deck_screws = [[-51.0, 27.5], [-51.0, -27.5], [57.0, 27.5], [57.0, -27.5]];
if want(only, "Deck") {
    studios.begin(d, "Deck", [95, 110, 69], "PLA");
    d.variable("deck_t", DECK_T);
    let zt = HULL_Z1 + DECK_T;
    let base = f::plane(d, "z", HULL_Z1);
    let s = d.sketch(base, "Deck outline");
    // the hull's glacis shows between the fenders; fender ends cut at 45°
    s.polygon([[63, -33], [76, -33], [76, -51], [70, -57], [-70, -57], [-76, -51], [-76, -33], [-64, -33], [-64, 33], [-76, 33], [-76, 51],
               [-70, 57], [70, 57], [76, 51], [76, 33], [63, 33]]);
    f::ext_expr(d, s, "z", "deck_t", true, "new", "Deck");
    let top = [];
    for e in d.edges() { if (e.start[2] - zt).abs() < 0.01 && (e.end[2] - zt).abs() < 0.01 { top.push(e.ref); } }
    d.feature("chamfer", #{edges: top, distance: 0.8}, "Top edge chamfer");
    // skirts over the top run of the tracks (left, mirrored to the right)
    let s = f::sketch_at(d, "y", 55.4, "Skirt");
    // (it reaches 1 mm up into the deck so it joins it)
    s.polygon([[-62, HULL_Z1 + 1.0], [64, HULL_Z1 + 1.0], [64, HULL_Z1], [59, 31], [-57, 31], [-62, HULL_Z1]]);
    let sk = f::ext(d, s, "y", 55.4, 57.0, "add", "Skirt");
    f::mirror(d, [sk], "Front", "Skirt (right)");
    // turret SG90 mounting bosses for its tab screws (M2 pilots), 0.65 clear of the servo body
    let s = d.sketch(base, "Servo bosses");
    s.rect_corners([-5, -10.85], [5, -6.55]);
    s.rect_corners([-5, 17.25], [5, 21.55]);
    f::ext(d, s, "z", HULL_Z1, HULL_Z1 - 4.0, "add", "Servo bosses");
    let s = d.sketch(base, "Servo screw pilots");
    s.circle([0, -8.45], 1.6);
    s.circle([0, 19.15], 1.6);
    f::ext(d, s, "z", HULL_Z1, HULL_Z1 - 4.5, "remove", "Servo screw pilots");
    // opening for the servo spline and the slot for the camera ribbon and wires
    let s = d.sketch(base, "Openings");
    s.circle([0, 0], 12);
    s.rect_corners([19, -8], [25, 8]);
    f::ext_sym(d, s, 20, "remove", "Openings");
    // countersunk M3 screws into the hull's bosses
    let pts = [];
    for p in deck_screws { pts.push(#{kind: "vertex", point: [p[0], p[1], zt]}); }
    d.feature("hole", #{points: pts, hole_type: "countersink", diameter: 3.4, cs_diameter: 6.6, cs_angle: 90, through: true}, "Deck screws");
    // engraved engine grille behind the turret and a driver's hatch at the front, 0.6 deep
    let s = f::sketch_at(d, "z", zt, "Engraving");
    for y in [-16.0, -8.0, 0.0, 8.0, 16.0] { s.rect_corners([-62, y - 1.2], [-44, y + 1.2]); }
    s.circle([46, -14], 15);
    s.circle([46, -14], 13);
    let regs = [];
    for y in [-16.0, -8.0, 0.0, 8.0, 16.0] { regs.push(f::region(s, -53.0, y)); }
    regs.push(f::region(s, 46.0 + 7.0, -14.0));
    f::ext(d, regs, "z", zt, zt - 0.6, "remove", "Engraving");
    // the hatch's centre dot in a sketch of its own: rCAD loses a disc nested inside a ring's hole
    // when both are extruded together
    let s = d.sketch(f::plane(d, "z", zt), "Hatch dot");
    s.circle([46, -14], 3);
    f::ext(d, s, "z", zt, zt - 0.6, "remove", "Hatch dot");
    studios.finish(d, ["deck"], out);
}

// ---------------------------------------------------------------- turret shell
// faceted walls lofted from the base outline to a smaller roof, shelled 2 mm, open underneath; the
// gun drum turns in a round embrasure at the front; prints roof down
fn body_of(d, fid) {
    for p in d.parts() { if p.body[0] == fid { return p.body; } }
    throw "no body from feature " + fid;
}
if want(only, "Turret shell") {
    studios.begin(d, "Turret shell", [95, 110, 69], "PLA");
    d.variable("wall", 2);
    let base_pts = [[30, -12], [30, 12], [18, 32], [-24, 32], [-40, 18], [-40, -18], [-24, -32], [18, -32]];
    let roof_pts = [[25, -10], [25, 10], [15, 27], [-22, 27], [-36, 15], [-36, -15], [-22, -27], [15, -27]];
    let pb = f::plane(d, "z", ring_z1);
    let pr = f::plane(d, "z", shell_z1);
    let sb = d.sketch(pb, "Base outline"); sb.polygon(base_pts);
    let sr = d.sketch(pr, "Roof outline"); sr.polygon(roof_pts);
    let loft = d.feature("loft", #{profiles: [sb, sr], ruled: true, op: "new"}, "Turret");
    let shell_body = body_of(d, loft);
    d.feature("shell", #{faces: [#{kind: "face", point: [-5.0, 0.0, ring_z1], normal: [0, 0, -1]}], thickness: "wall"}, "Shell");
    let roof_e = [];
    for e in d.edges() { if (e.start[2] - shell_z1).abs() < 0.01 && (e.end[2] - shell_z1).abs() < 0.01 { roof_e.push(e.ref); } }
    d.feature("chamfer", #{edges: roof_e, distance: 1.2}, "Roof chamfer");
    // embrasure the gun drum turns in (0.3 clear), open below it so nothing bridges over the drum
    let s = f::sketch_at(d, "y", -14.7, "Embrasure");
    s.circle([gx, gz], 2.0 * (DRUM_R + 0.3));
    s.rect_corners([gx - 6.0, ring_z1 - 1.0], [gx + 20.0, gz]);
    f::ext(d, s, "y", -14.7, 14.7, "remove", "Embrasure");
    // hole for the cupola's spigot; the hit LED (WS2812) sits under it
    let s = d.sketch(pr, "Cupola hole");
    s.circle([-14, 12], 6.7);
    f::ext(d, s, "z", shell_z1, shell_z1 - 5.0, "remove", "Cupola hole");
    // TSOP38238 windows for its 6.0 × 7.0 body (glued in): 6.4 wide, 7.4 tall, at ±45° and ±135°
    let s = f::sketch_at(d, "z", 68.3, "TSOP window");
    let w = [];
    for p in [[22.0, -3.2], [50.0, -3.2], [50.0, 3.2], [22.0, 3.2]] {
        let a = 45.0.to_radians();
        w.push([p[0] * cos(a) - p[1] * sin(a), p[0] * sin(a) + p[1] * cos(a)]);
    }
    s.polygon(w);
    let win = f::ext(d, s, "z", 68.3, 75.7, "remove", "TSOP window");
    f::cpattern(d, [win], "z", 4, "TSOP windows");
    // trunnion brackets for the gun drum's M2 pivot screws and the elevation servo's brackets, hanging
    // from the roof and trimmed to the outside of the walls
    let trim = d.feature("loft", #{profiles: [sb, sr], ruled: true, op: "new"}, "Trim volume");
    let trim_body = body_of(d, trim);
    let brackets = [];
    for b in [[[12.0, 14.9], [32.0, 16.9], 52.0, "Trunnion bracket L"], [[12.0, -16.9], [32.0, -14.9], 52.0, "Trunnion bracket R"]] {
        let s = f::sketch_at(d, "y", b[0][1], b[3]);
        s.rect_corners([b[0][0], b[2]], [b[1][0], shell_z1]);
        brackets.push(f::ext(d, s, "y", b[0][1], b[1][1], "new", b[3]));
    }
    // the elevation servo (SG90, output along +Y at x = -10, z = 65.5): brackets for its tabs, 0.65 clear of its body
    for b in [[-31.55, -27.25, "Servo bracket rear"], [-3.45, 0.85, "Servo bracket front"]] {
        let s = f::sketch_at(d, "y", -3.5, b[2]);
        s.rect_corners([b[0], 59.0], [b[1], shell_z1]);
        brackets.push(f::ext(d, s, "y", -3.5, 1.0, "new", b[2]));
    }
    let bodies = [];
    for b in brackets {
        let id = body_of(d, b);
        d.feature("boolean", #{kind: "intersect", target: #{kind: "body", body: id}, tools: [#{kind: "body", body: trim_body}], keep_tools: true}, "Trim");
        bodies.push(#{kind: "body", body: id});
    }
    d.feature("deleteBody", #{bodies: [#{kind: "body", body: trim_body}]}, "Delete trim volume");
    d.feature("boolean", #{kind: "union", target: #{kind: "body", body: shell_body}, tools: bodies}, "Join brackets");
    let s = f::sketch_at(d, "y", -4.5, "Servo screw pilots");
    s.circle([-29.15, gz], 1.6);
    s.circle([-1.55, gz], 1.6);
    f::ext(d, s, "y", -4.5, 1.5, "remove", "Servo screw pilots");
    // M2 pivot holes through the brackets and the side walls (the screws go in from outside)
    let s = d.sketch("front", "Pivot holes");
    s.circle([gx, gz], 2.4);
    f::ext_sym(d, s, 80, "remove", "Pivot holes");
    // 4 tubes from the ring to the roof: an M2 × 6 countersunk screw drops in from the roof and seats
    // on a 45° cone 2 mm above the ring (printed roof down, the cone is the only overhang)
    let s = d.sketch(pb, "Screw tubes");
    for p in tur_screws { s.circle(p, 5.2); }
    f::ext(d, s, "z", ring_z1, shell_z1, "add", "Screw tubes");
    let pts = [];
    for p in tur_screws { pts.push(#{kind: "vertex", point: [p[0], p[1], ring_z1 + 3.0]}); }
    d.feature("hole", #{points: pts, hole_type: "countersink", diameter: 2.4, depth: 4, through: false, cs_diameter: 4.4, cs_angle: 90}, "Screw holes");
    studios.finish(d, ["turret shell"], out);
}

// ---------------------------------------------------------------- gun drum
// holds the Pi camera (looking out of the front window) and the barrel; turns on two M2 screws in the
// turret's trunnion brackets; open at the back for the camera and its ribbon. Printed as two halves
// (split at y = 0), each lying on its outer side face, glued together
if want(only, "Gun drum") {
    studios.begin(d, "Gun drum", [95, 110, 69], "PLA");
    d.variable("drum_r", DRUM_R);
    d.variable("drum_half_w", DW);
    d.variable("drum_wall", 1.6);
    let cam_face = gx + 5.5;   // front face of the camera board
    let s = d.sketch("front", "Drum");
    s.circle([gx, gz], "2 * drum_r");
    f::ext_sym(d, s, "2 * drum_half_w", "new", "Drum");
    let s = d.sketch("front", "Inside");
    s.circle([gx, gz], "2 * (drum_r - drum_wall)");
    s.rect_corners([0, 40], [gx - 4.5, 90]);
    f::ext_sym(d, s, "2 * (drum_half_w - drum_wall)", "remove", "Inside and back opening");
    // plate across the front inside, for the camera's standoffs (reaching into the walls so it joins them)
    let s = d.sketch("front", "Camera plate");
    s.rect_corners([cam_face + 2.0, gz - 14.0], [cam_face + 4.0, gz + 13.0]);
    s.circle([gx, gz], 2.0 * (DRUM_R - 0.8));
    f::ext_sym(d, [f::region(s, cam_face + 3.0, gz)], 2.0 * (DW - 0.8), "add", "Camera plate");
    let s = f::sketch_at(d, "y", -5.2, "Lens opening");
    s.rect_corners([cam_face, CAM_Z - 5.2], [cam_face + 5.0, CAM_Z + 5.2]);
    f::ext(d, s, "y", -5.2, 5.2, "remove", "Lens opening");
    // M2 standoffs for the camera board, with ribs out to the side walls so they print without support
    let st = f::plane(d, "x", cam_face);
    let s = d.sketch(st, "Standoffs");
    for y in [-10.5, 10.5] { for z in [CAM_Z, CAM_Z - 12.5] {
        s.circle([y, z], 4.0);
        let y1 = if y > 0.0 { DW - 1.0 } else { -DW + 1.0 };
        s.rect_corners([y.min(y1), z - 2.0], [y.max(y1), z + 2.0]);
    } }
    f::ext(d, s, "x", cam_face, cam_face + 2.2, "add", "Standoffs");
    let s = d.sketch(st, "Standoff pilots");
    for y in [-10.5, 10.5] { for z in [CAM_Z, CAM_Z - 12.5] { s.circle([y, z], 1.6); } }
    f::ext(d, s, "x", cam_face, cam_face + 2.0, "remove", "Standoff pilots");
    // collar round the barrel socket, with a 45° diamond and a web out to the side walls under it (so
    // it prints without support in the split halves), trimmed to 6 mm outside the drum
    let s = f::sketch_at(d, "x", gx + 12.0, "Collar");
    let k = 6.8 * 2.0.sqrt();
    s.circle([0, BARREL_Z], 13.6);
    s.polygon([[-k, BARREL_Z], [-(k - 6.8), BARREL_Z - 6.8], [k - 6.8, BARREL_Z - 6.8], [k, BARREL_Z], [k - 6.8, BARREL_Z + 6.8], [-(k - 6.8), BARREL_Z + 6.8]]);
    s.rect_corners([-DW, BARREL_Z - 1.5], [DW, BARREL_Z + 1.5]);
    f::ext(d, s, "x", gx + 12.0, gx + 15.5, "add", "Collar");
    let s = d.sketch("front", "Collar trim");
    s.circle([gx, gz], 2.0 * (DRUM_R + 6.0));
    s.rect_corners([gx + 10.0, 30], [gx + 20.0, 100]);
    f::ext_sym(d, [f::region(s, gx + 15.0, 35.0)], 40, "remove", "Collar trim");
    // camera window
    let s = f::sketch_at(d, "x", gx + 6.0, "Camera window");
    s.circle([0, CAM_Z], 9.0);
    f::ext(d, s, "x", gx + 6.0, gx + 21.0, "remove", "Camera window");
    // barrel socket, 4.5 deep with a 0.25 glue gap, countersunk for the barrel's conical flange
    let s = f::sketch_at(d, "x", gx + 11.0, "Barrel socket");
    s.circle([0, BARREL_Z], 10.5);
    f::ext(d, s, "x", gx + 11.0, gx + 18.0, "remove", "Barrel socket");
    let mouth = [];
    for e in d.edges() { if (e.start[0] - gx - 15.5).abs() < 0.01 && (e.length_mm - 2.0 * PI() * 5.25).abs() < 0.05 { mouth.push(e.ref); } }
    d.feature("chamfer", #{edges: mouth, distance: 1.05}, "Socket countersink");
    let s = f::sketch_at(d, "x", gx, "LED leads");
    s.circle([0, BARREL_Z], 3.0);
    f::ext(d, s, "x", gx, gx + 14.0, "remove", "LED leads");
    let s = d.sketch("front", "Pivot holes");
    s.circle([gx, gz], 1.6);
    f::ext_sym(d, s, 2.0 * DW + 2.0, "remove", "Pivot holes");
    // pushrod: a blind hole in the left wall, the wire's L-bend sits in it
    let s = f::sketch_at(d, "y", DW - 3.0, "Pushrod hole");
    s.circle([gx, gz - 10.0], 1.6);
    f::ext(d, s, "y", DW - 3.0, DW - 0.3, "remove", "Pushrod hole");
    let drum = 0;
    for p in d.parts() { drum = p.body; }
    d.feature("split", #{target: #{kind: "body", body: drum}, plane: #{kind: "origin_plane", plane: "Front"}}, "Split into halves");
    let ps = d.parts();
    // left half first (y > 0)
    let names = if ps[0].centroid_mm[1] > 0.0 { ["gun drum left", "gun drum right"] } else { ["gun drum right", "gun drum left"] };
    studios.finish(d, names, out);
}

// ---------------------------------------------------------------- hull tub
// side profile (glacis plates at the front, sloped rear plate) cut to a rounded plan, shelled 2.4 mm
// open at the top; bosses, pads and standoffs inside
// a rounded rectangle from lines and arcs: centre (cx, cy), size w × h, corner radius r
fn rounded_rect(s, cx, cy, w, h, r) {
    let x0 = cx - w / 2.0; let x1 = cx + w / 2.0; let y0 = cy - h / 2.0; let y1 = cy + h / 2.0;
    s.line([x0 + r, y0], [x1 - r, y0]);
    s.arc([x1 - r, y0 + r], [x1 - r, y0], [x1, y0 + r]);
    s.line([x1, y0 + r], [x1, y1 - r]);
    s.arc([x1 - r, y1 - r], [x1, y1 - r], [x1 - r, y1]);
    s.line([x1 - r, y1], [x0 + r, y1]);
    s.arc([x0 + r, y1 - r], [x0 + r, y1], [x0, y1 - r]);
    s.line([x0, y1 - r], [x0, y0 + r]);
    s.arc([x0 + r, y0 + r], [x0, y0 + r], [x0 + r, y0]);
}
if want(only, "Hull") {
    studios.begin(d, "Hull", [78, 91, 56], "PLA");
    d.variable("wall", WALL);
    let y_in = HULL_Y - WALL;
    let s = d.sketch("front", "Side profile");
    // the end plates run 1 mm past the plan outline (which trims them to x = -71 and 73): coplanar
    // faces from the two cuts would leave split faces that the shell can't offset
    s.polygon([[-72, HULL_Z0], [67, HULL_Z0], [74, 12.0 + 7.0 / 6.0], [74, 26.8], [63, HULL_Z1], [-64, HULL_Z1], [-72, 31.0 - 9.0 / 7.0]]);
    f::ext_sym(d, s, 80, "new", "Hull");
    let s = f::sketch_at(d, "z", HULL_Z0 - 1.0, "Plan");
    rounded_rect(s, 1.0, 0.0, 144.0, 2.0 * HULL_Y, 5.0);
    f::ext(d, s, "z", HULL_Z0 - 1.0, HULL_Z0 + 49.0, "intersect", "Plan");
    d.feature("shell", #{faces: [#{kind: "face", point: [0.0, 0.0, HULL_Z1], normal: [0, 0, 1]}], thickness: "wall"}, "Shell");
    // road wheel axle bosses (M3 screws threaded into Ø2.6), reaching into the wall so they join it
    let s = f::sketch_at(d, "y", y_in + 0.4, "Axle bosses");
    for x in RW_X { s.circle([x, RW_Z], 9.0); }
    let ab = f::ext(d, s, "y", y_in + 0.4, y_in - 6.0, "add", "Axle bosses");
    // idler pad behind the tension slot, its underside sloped 45° into the wall (prints without support)
    let s = f::sketch_at(d, "x", IDL[0] - 4.4, "Idler pad");
    // (it reaches 0.4 into the side wall and 0.3 into the front wall, so it joins them)
    s.polygon([[y_in + 0.4, 11.1], [y_in + 0.4, 26.0], [y_in - 5.0, 26.0], [y_in - 5.0, 16.5]]);
    let pad = f::ext(d, s, "x", IDL[0] - 4.4, IDL[0] + 5.5, "add", "Idler pad");
    // a channel on its inside face holds the M3 nut so it can't turn
    let s = f::sketch_at(d, "x", IDL[0] - 4.4, "Nut channel");
    s.rect_corners([y_in - 6.0, IDL[1] - 2.95], [y_in - 2.4, IDL[1] + 2.95]);
    let nut = f::ext(d, s, "x", IDL[0] - 4.4, IDL[0] + 5.2, "remove", "Nut channel");
    // motor cradle, and the gearbox recessed 1.4 into the wall above it
    let s = f::sketch_at(d, "y", 9.0, "Motor cradle");
    s.rect_corners([SPR[0] - 6.0, HULL_Z0 + WALL - 0.5], [SPR[0] + 6.0, SPR[1] - 5.2]);
    let mc = f::ext(d, s, "y", 9.0, y_in + 0.4, "add", "Motor cradle");
    let s = f::sketch_at(d, "y", y_in - 1.0, "Motor recess");
    s.rect_corners([SPR[0] - 6.25, SPR[1] - 5.25], [SPR[0] + 6.25, SPR[1] + 5.25]);
    let mr = f::ext(d, s, "y", y_in - 1.0, HULL_Y - 1.0, "remove", "Motor recess");
    f::mirror(d, [ab, pad, nut, mc, mr], "Front", "Right side");
    // standoffs for the Radxa Zero 3W (M2.5, 58 × 23) from the right wall: round posts with a 45°
    // point underneath so they print without support (before the axle holes: one passes the corner
    // of the rear lower standoff)
    let s = f::sketch_at(d, "y", -y_in - 0.5, "Board standoffs");
    let k = 2.6 * 2.0.sqrt();
    for x in [-40.0, 18.0] { for z in [13.0, 36.0] {
        s.circle([x, z], 5.2);
        s.polygon([[x - k, z], [x, z - k], [x + k, z], [x, z + k]]);
    } }
    f::ext(d, s, "y", -y_in - 0.5, -19.9, "add", "Board standoffs");
    let s = f::sketch_at(d, "y", -y_in - 1.0, "Standoff pilots");
    for x in [-40.0, 18.0] { for z in [13.0, 36.0] { s.circle([x, z], 2.1); } }
    f::ext(d, s, "y", -y_in - 1.0, -y_in + 8.0, "remove", "Standoff pilots");
    // through holes: road wheel axles, motor shaft and its two M1.6 screws, idler tension slots
    let s = d.sketch("front", "Side holes");
    for x in RW_X { s.circle([x, RW_Z], 2.6); }
    s.circle([SPR[0], SPR[1]], 4.6);
    for dx in [-4.5, 4.5] { s.circle([SPR[0] + dx, SPR[1]], 1.9); }
    s.line([IDL[0] - 3.0, IDL[1] - 1.7], [IDL[0] + 3.0, IDL[1] - 1.7]);
    s.arc([IDL[0] + 3.0, IDL[1]], [IDL[0] + 3.0, IDL[1] - 1.7], [IDL[0] + 3.0, IDL[1] + 1.7]);
    s.line([IDL[0] + 3.0, IDL[1] + 1.7], [IDL[0] - 3.0, IDL[1] + 1.7]);
    s.arc([IDL[0] - 3.0, IDL[1]], [IDL[0] - 3.0, IDL[1] + 1.7], [IDL[0] - 3.0, IDL[1] - 1.7]);
    f::ext_sym(d, s, 2.0 * HULL_Y + 2.0, "remove", "Side holes");
    // battery strap slots in the floor
    let s = f::sketch_at(d, "z", HULL_Z0 - 1.0, "Strap slots");
    for sy in [1.0, -1.0] { s.rect_corners([34, sy * 17.0 - 1.5], [46, sy * 17.0 + 1.5]); }
    f::ext(d, s, "z", HULL_Z0 - 1.0, HULL_Z0 + WALL + 1.0, "remove", "Strap slots");
    // deck screw bosses (M3 self-tapping, Ø2.5)
    let s = f::sketch_at(d, "z", HULL_Z0 + 1.0, "Deck screw bosses");
    for p in deck_screws { s.circle(p, 7.0); }
    f::ext(d, s, "z", HULL_Z0 + 1.0, HULL_Z1, "add", "Deck screw bosses");
    let s = d.sketch(f::plane(d, "z", HULL_Z1), "Deck screw pilots");
    for p in deck_screws { s.circle(p, 2.5); }
    f::ext(d, s, "z", HULL_Z1, HULL_Z1 - 14.0, "remove", "Deck screw pilots");
    // power switch in the rear wall: KCD11 rocker, upright, 8.7 × 13.2 snap-in cutout
    let s = f::sketch_at(d, "x", -66.0, "Switch cutout");
    s.rect_corners([-4.35, 17.0], [4.35, 30.2]);
    f::ext(d, s, "x", -66.0, -74.0, "remove", "Switch cutout");
    // rCAD leaves some of the added bosses as separate bodies (its overlap test fails on this shelled
    // part), so join everything into the tub
    let ps = d.parts();
    if ps.len() > 1 {
        let tools = [];
        for i in 1..ps.len() { tools.push(#{kind: "body", body: ps[i].body}); }
        d.feature("boolean", #{kind: "union", target: #{kind: "body", body: ps[0].body}, tools: tools}, "Join");
    }
    studios.finish(d, ["hull tub"], out);
}

// ---------------------------------------------------------------- tracks
// a stadium of lines and arcs, centred on the origin: straight length s, end radius r
fn stadium(s, l, r, y) {
    s.line([-l / 2.0, y - r], [l / 2.0, y - r]);
    s.arc([l / 2.0, y], [l / 2.0, y - r], [l / 2.0, y + r]);
    s.line([l / 2.0, y + r], [-l / 2.0, y + r]);
    s.arc([-l / 2.0, y], [-l / 2.0, y + r], [-l / 2.0, y - r]);
}
// the same as a closed path for a curve pattern, starting at distance t along the bottom straight
fn stadium_path(s, l, r, t) {
    let x0 = -l / 2.0 + t;
    let c = [];
    c.push(s.line([x0, -r], [l / 2.0, -r]));
    c.push(s.arc([l / 2.0, 0], [l / 2.0, -r], [l / 2.0, r]));
    c.push(s.line([l / 2.0, r], [-l / 2.0, r]));
    c.push(s.arc([-l / 2.0, 0], [-l / 2.0, r], [-l / 2.0, -r]));
    c.push(s.line([-l / 2.0, -r], [x0, -r]));
    let refs = [];
    for x in c { refs.push(#{kind: "sketch_curve", sketch: s.id, curve: if "line" in x { x.line } else { x.arc }}); }
    refs
}
// The track as printed: a TPU loop standing on the bed, lugs inside (their lower face at 45°) and
// grousers outside, spaced by arc length along the band's mid-plane (the 8 mm pitch line)
if want(only, "Track") {
    studios.begin(d, "Track", [30, 30, 30], "");
    d.variable("lugs", LUGS);
    d.variable("pitch", PITCH);
    d.variable("band", BAND);
    d.variable("track_w", TRACK_W);
    let r = 25.0;
    let rm = r + BAND / 2.0;
    let l = (LUGS * PITCH - 2.0 * PI() * rm) / 2.0;
    let w = TRACK_W;
    let s = d.sketch("top", "Band");
    stadium(s, l, r + BAND, 0.0);
    stadium(s, l, r, 0.0);
    f::ext_expr(d, [f::region(s, 0.0, -r - BAND / 2.0)], "z", "track_w", true, "new", "Band");
    // first lug at 4 mm along the bottom straight, first grouser at 8 mm
    let x = -l / 2.0 + PITCH / 2.0;
    let s = f::sketch_at(d, "x", x - 2.0, "Lug");
    s.polygon([[-r - 0.2, w / 2.0 - 5.7], [-r + 2.5, w / 2.0 - 3.0], [-r + 2.5, w / 2.0 + 3.0], [-r - 0.2, w / 2.0 + 3.0]]);
    let lug = f::ext(d, s, "x", x - 2.0, x + 2.0, "add", "Lug");
    let x = -l / 2.0 + PITCH;
    let s = d.sketch("top", "Grouser");
    s.polygon([[x - 1.2, -r - BAND + 0.3], [x + 1.2, -r - BAND + 0.3], [x + 1.0, -r - BAND - 1.0], [x - 1.0, -r - BAND - 1.0]]);
    let gr = f::ext_expr(d, s, "z", "track_w", true, "add", "Grouser");
    let p = d.sketch("top", "Lug path");
    let lp = stadium_path(p, l, rm, PITCH / 2.0);
    d.feature("curvePattern", #{kind: "feature", bodies: [#{kind: "feature", feature: lug}], path: lp, count: "lugs", align: true}, "Lugs");
    let p = d.sketch("top", "Grouser path");
    let gp = stadium_path(p, l, rm, PITCH);
    d.feature("curvePattern", #{kind: "feature", bodies: [#{kind: "feature", feature: gr}], path: gp, count: "lugs", align: true}, "Grousers");
    studios.finish(d, ["track"], out);
}

// The track as it runs round the wheels, for the assembly, with its 42 grousers outside and lugs inside
// at the 8 mm pitch on the band's mid-plane.
// Its inner surface is the belt round the sprocket, road wheels and idler, 0.1 out
fn belt(s, circles, grow) { belt_from(s, circles, grow, ()) }
// the same, as a path for a curve pattern that starts (and ends) on the first circle at angle `start`
// (degrees in the sketch, which must lie on that circle's wrap); without one it is a plain loop
fn belt_from(s, circles, grow, start) {
    // circles in counter-clockwise order (x, z, r) round the outline; tangent n is outwards
    let n = circles.len();
    let tp = [];
    for i in 0..n {
        let a = circles[i]; let b = circles[(i + 1) % n];
        let ra = a[2] + grow; let rb = b[2] + grow;
        let dx = b[0] - a[0]; let dz = b[1] - a[1]; let l = (dx * dx + dz * dz).sqrt();
        let al = -((ra - rb) / l).acos();
        let ux = dx / l; let uz = dz / l;
        let nx = ux * al.cos() - uz * al.sin(); let nz = ux * al.sin() + uz * al.cos();
        tp.push([[a[0] + ra * nx, a[1] + ra * nz], [b[0] + rb * nx, b[1] + rb * nz]]);
    }
    let c0 = circles[0];
    let r0 = c0[2] + grow;
    let p0 = if start == () { () } else { [c0[0] + r0 * start.to_radians().cos(), c0[1] + r0 * start.to_radians().sin()] };
    let refs = [];
    let cv = |c| #{kind: "sketch_curve", sketch: s.id, curve: if "line" in c { c.line } else { c.arc }};
    if p0 != () { refs.push(cv.call(s.arc([c0[0], c0[1]], p0, tp[0][0]))); }
    for i in 0..n {
        refs.push(cv.call(s.line(tp[i][0], tp[i][1])));
        let c = circles[(i + 1) % n];
        let e = if i == n - 1 && p0 != () { p0 } else { tp[(i + 1) % n][0] };
        refs.push(cv.call(s.arc([c[0], c[1]], tp[i][1], e)));
    }
    refs
}
// a quadrilateral given in (along, out) coordinates from the point at angle a (degrees) on circle c,
// radius r, going counter-clockwise
fn wrap_quad(s, c, r, a, pts) {
    let ca = a.to_radians().cos(); let sa = a.to_radians().sin();
    let q = [];
    for p in pts { q.push([c[0] + (r + p[1]) * ca - p[0] * sa, c[1] + (r + p[1]) * sa + p[0] * ca]); }
    s.polygon(q);
}
if want(only, "Track (as fitted)") {
    studios.begin(d, "Track (as fitted)", [30, 30, 30], "");
    let circles = [[SPR[0], SPR[1], SPR_R], [RW_X[0], RW_Z, RW_R], [RW_X[3], RW_Z, RW_R], [IDL[0], IDL[1], IDL_R]];
    let s = f::sketch_at(d, "y", TRACK_Y0, "Band");
    belt(s, circles, 0.1);
    belt(s, circles, 0.1 + BAND);
    let band = f::ext(d, [f::region(s, 0.0, 1.1)], "y", TRACK_Y0, TRACK_Y0 + TRACK_W, "new", "Band");
    // a lug sits in each sprocket pocket (the first pocket is at the top, then every 360/10°, one pitch
    // along the pitch line); the grousers are half a pitch from them. Each seed sits at the start of its
    // own pitch-line path, on the sprocket's wrap at the back.
    let spr = circles[0];
    let rp = SPR_R + 0.1 + BAND / 2.0;
    let h = BAND / 2.0;
    let la = 90.0 + 2.0 * 36.0;
    let ga = la + 18.0;
    let s = f::sketch_at(d, "y", TRACK_Y0, "Grouser");
    wrap_quad(s, spr, rp, ga, [[-1.2, h - 0.3], [1.2, h - 0.3], [1.0, h + 1.0], [-1.0, h + 1.0]]);
    let gr = f::ext(d, s, "y", TRACK_Y0, TRACK_Y0 + TRACK_W, "add", "Grouser");
    // the lug, 4 long, 2.5 tall, its inner 6 mm of width (the 45° chamfer is left out)
    let s = f::sketch_at(d, "y", TRACK_Y0 + TRACK_W / 2.0 - 3.0, "Lug");
    wrap_quad(s, spr, rp, la, [[-2.0, -h + 0.2], [2.0, -h + 0.2], [2.0, -h - 2.5], [-2.0, -h - 2.5]]);
    let lug = f::ext(d, s, "y", TRACK_Y0 + TRACK_W / 2.0 - 3.0, TRACK_Y0 + TRACK_W / 2.0 + 3.0, "add", "Lug");
    let p = f::sketch_at(d, "y", TRACK_Y0, "Grouser path");
    d.feature("curvePattern", #{kind: "feature", bodies: [#{kind: "feature", feature: gr}], path: belt_from(p, circles, 0.1 + h, ga), count: LUGS, align: true}, "Grousers");
    let p = f::sketch_at(d, "y", TRACK_Y0, "Lug path");
    d.feature("curvePattern", #{kind: "feature", bodies: [#{kind: "feature", feature: lug}], path: belt_from(p, circles, 0.1 + h, la), count: LUGS, align: true}, "Lugs");
    d.feature("mirror", #{kind: "part", bodies: [#{kind: "body", body: body_of(d, band)}], plane: #{kind: "origin_plane", plane: "Front"}}, "Right track");
    studios.finish(d, ["track L (as fitted)", "track R (as fitted)"], out);
}

// ---------------------------------------------------------------- bought parts
// stand-ins with their outline sizes, imported from parts.rhai's "project" output (third argument: that
// folder, with list.json), where they sit in the tank
let bought = [];
if ARGS.len() > 2 && (only == "" || only == "Bought parts") {
    let dir = ARGS[2] + "/";
    let want_b = ["motor L", "motor R", "servo", "elevation servo", "elevation horn", "pushrod", "battery", "drv8833", "buck",
                  "radxa zero 3w", "rp2040-zero", "pi camera", "ir-led"];
    studios.begin(d, "Bought parts", [200, 160, 64], "");
    let list = read_json(dir + "list.json");
    let parts = [];
    for e in list {
        if !want_b.contains(e.name) { continue; }
        d.tool("import_file", #{path: dir + e.file});
        let ps = d.parts();
        let h = e.color;
        let rgb = [parse_int(h.sub_string(1, 2), 16), parse_int(h.sub_string(3, 2), 16), parse_int(h.sub_string(5, 2), 16)];
        let pe = #{body: ps[ps.len() - 1].body, name: e.name, color: rgb};
        if e.name == "pushrod" { pe.material = "Steel"; }
        parts.push(pe);
    }
    d.tool("set_parts", #{parts: parts});
    let v = [];
    for p in d.parts() { v.push(#{name: p.name, volume: p.volume_mm3, body: p.body, centroid: p.centroid_mm}); }
    let s = studios[studios.len() - 1];
    s.parts = v;
    studios[studios.len() - 1] = s;
}

// ---------------------------------------------------------------- assembly
// the hull is fixed; the wheels turn on their axles, the turret on the servo (±90°), the gun on its
// pivot (-6° … +20°); mates use explicit connectors in each part's own coordinates
if only == "" {
    let find = |name| { for s in studios { for p in s.parts { if p.name == name { return #{studio: s.studio, body: p.body}; } } } throw "no part " + name; };
    d.tool("create_assembly", #{name: "FPV Tank"});
    let A = 0;
    let insert = |name, label, pos, rot, fixed| {
        let p = find.call(name);
        let a = #{assembly: 0, studio: p.studio, body: p.body, position: pos, fixed: fixed, name: label};
        if rot != () { a.rotate = rot; }
        d.tool("assembly_insert", a).instance
    };
    let mate = |kind, ia, oa, za, xa, ib, ob, zb, xb, extra| {
        let m = #{assembly: 0, kind: kind, flip: true, a: #{instance: ia, origin: oa, z: za, x: xa}, b: #{instance: ib, origin: ob, z: zb, x: xb}};
        for k in extra.keys() { m[k] = extra[k]; }
        d.tool("assembly_mate", m)
    };
    let X = [1, 0, 0]; let Y = [0, 1, 0]; let Z = [0, 0, 1]; let O = [0, 0, 0];
    let flipx = #{axis: [1, 0, 0], angle: 180};
    let hull = insert.call("hull tub", "hull tub", O, (), true);
    let deck = insert.call("deck", "deck", O, (), true);
    // drivetrain: left side as modelled, right side turned half a turn about X (the D flat stays at +X)
    for side in [1.0, -1.0] {
        let tag = if side > 0.0 { " L" } else { " R" };
        let rot = if side > 0.0 { () } else { flipx };
        let wheels = [["sprocket", SPR[0], SPR[1]], ["idler", IDL[0], IDL[1]]];
        for x in RW_X { wheels.push(["road wheel", x, RW_Z]); }
        let n = 0;
        for w in wheels {
            n += 1;
            let label = w[0] + tag + (if w[0] == "road wheel" { " " + (n - 2) } else { "" });
            let i = insert.call(w[0], label, [w[1], 0.0, w[2]], rot, false);
            mate.call("revolute", hull, [w[1], 0.0, w[2]], [0.0, side, 0.0], X, i, O, Y, X, #{name: label + " axle"});
        }
        insert.call("track" + tag + " (as fitted)", "track" + tag, O, (), true);
    }
    // Each connector sits on the axis of a hole or boss in both parts: rCAD moves a connector onto
    // the part's geometry when it rebuilds, so one in empty space would drift onto some other face.
    // turret: the ring turns on the servo (about the deck's spline opening), the shell is screwed to it
    let ring = insert.call("turret ring", "turret ring", O, (), false);
    mate.call("revolute", deck, [0, 0, TUR_Z0], Z, X, ring, [0, 0, TUR_Z0], Z, X, #{name: "turret", limits: [-90, 90]});
    let shell = insert.call("turret shell", "turret shell", O, (), false);
    let t0 = [tur_screws[0][0], tur_screws[0][1], ring_z1];
    mate.call("fastened", ring, t0, Z, X, shell, t0, Z, X, #{name: "shell on ring"});
    let cup = insert.call("cupola", "cupola", [-14.0, 12.0, shell_z1], (), false);
    mate.call("fastened", shell, [-14.0, 12.0, shell_z1], Z, X, cup, O, Z, X, #{name: "cupola"});
    // gun: the drum turns on its pivot screws; positive rotation about +Y lowers the barrel
    let gl = insert.call("gun drum left", "gun drum left", O, (), false);
    mate.call("revolute", shell, [gx, 0, gz], Y, X, gl, [gx, 0, gz], Y, X, #{name: "gun elevation", limits: [-ELEV[1], -ELEV[0]]});
    let gr = insert.call("gun drum right", "gun drum right", O, (), false);
    mate.call("fastened", gl, [gx, 0, gz], Y, X, gr, [gx, 0, gz], Y, X, #{name: "drum halves glued"});
    let br = insert.call("barrel", "barrel", [xr, 0.0, BARREL_Z], (), false);
    mate.call("fastened", gl, [xr, 0, BARREL_Z], X, Z, br, O, X, Z, #{name: "barrel in socket"});
    // bought parts: the camera on its lens axis and the LED on the barrel axis in the drum, the
    // elevation servo on its tab screw; the rest sit fixed where they are
    for b in bought_on(studios) {
        let free = b.name == "pi camera" || b.name == "ir-led" || b.name == "elevation servo";
        let i = insert.call(b.name, b.name, O, (), !free);
        if b.name == "pi camera" { mate.call("fastened", gl, [gx + 10.0, 0, CAM_Z], X, Z, i, [gx + 10.0, 0, CAM_Z], X, Z, #{name: b.name}); }
        if b.name == "ir-led" { mate.call("fastened", gl, [xr, 0, BARREL_Z], X, Z, i, [xr, 0, BARREL_Z], X, Z, #{name: b.name}); }
        if b.name == "elevation servo" { mate.call("fastened", shell, [-1.55, 0, gz], Y, X, i, [-1.55, 0, gz], Y, X, #{name: b.name}); }
    }
    // standard screws: M3 × 30 axles for the road wheels, M3 × 35 and a nut for each idler,
    // countersunk M3 × 12 for the deck, M2 × 6 drum pivots (seating plane at z = 0, shank down -Z)
    let screw = |std, size, len, pos, rot, label| {
        let a = #{standard: std, size: size, position: pos, rotate: rot, fixed: true, name: label};
        if len > 0.0 { a.length = len; }
        d.tool("insert_standard_part", a).instance
    };
    for side in [1.0, -1.0] {
        let rot = #{axis: [1, 0, 0], angle: -90.0 * side};
        for x in RW_X { screw.call("ISO 4762", "M3", 30.0, [x, side * 53.5, RW_Z], rot, "axle screw"); }
        screw.call("ISO 4762", "M3", 35.0, [IDL[0], side * 53.5, IDL[1]], rot, "idler screw");
    }
    for p in deck_screws { screw.call("ISO 10642", "M3", 12.0, [p[0], p[1], HULL_Z1 + DECK_T], #{axis: Z, angle: 0}, "deck screw"); }
    for side in [1.0, -1.0] {
        // head on the shell's side wall (y = ±16.6 at the pivot), shank in towards the drum
        let i = screw.call("ISO 4762", "M2", 6.0, [gx, side * 16.6, gz], #{axis: [1, 0, 0], angle: -90.0 * side}, "pivot screw");
        d.tool("assembly_move", #{assembly: 0, instance: i, fixed: false});
        mate.call("fastened", shell, [gx, side * 16.6, gz], [0.0, side, 0.0], X, i, O, Z, X, #{name: "pivot screw"});
    }
}
fn bought_on(studios) {
    let on = #{"elevation servo": "turret", "elevation horn": "turret", pushrod: "turret", "pi camera": "gun", "ir-led": "gun"};
    let out = [];
    for s in studios { if s.name == "Bought parts" { for p in s.parts { out.push(#{name: p.name, on: if p.name in on { on[p.name] } else { "hull" }}); } } }
    out
}

d.save(out + "FPV Tank.json");
let vols = #{};
for s in studios { vols[s.name] = s.parts; }
vols
