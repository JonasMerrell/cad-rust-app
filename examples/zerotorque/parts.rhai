// Stand-in ZeroTorque parts (a mallet head, a hosel plug with a foam core), built with the
// scripting API and exported as STEP for the CAM and FEA examples:
//   rcad script examples/zerotorque/parts.rhai [output folder]
// Units mm. Head: X toe-heel, Y face (y = 0) to back, Z sole (z = 0) up.

let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };

// ---------------------------------------------------------------- head
let W = 110.0;  let D = 90.0;  let H = 25.0;  let R = 15.0;
let inset = H * tan(deg(6.0));                       // 6° drafted side walls
let sole = rounded_rect(vec3(W / 2, D / 2, 0), W, D, R);
let top = rounded_rect(vec3(W / 2, D / 2, H), W - 2 * inset, D - 2 * inset, R - inset);
let head = loft([sole, top], true);

// rear deck 6 mm lower, joined by a 45° step
let step = extrude(polygon([[-1, 48, 26], [-1, 55, 19], [-1, 96, 19], [-1, 96, 30], [-1, 48, 30]]), vec3(W + 2, 0, 0));
head = head.cut(step).unify();

// round the front deck's outer top edges (1.5 mm), picked by geometry and filleted in one go
let rounds = [];
for e in head.edges() {
    let fs = e.faces();
    if fs.len() != 2 { continue; }
    let m = e.mid();
    let n0 = fs[0].normal(m);  let n1 = fs[1].normal(m);
    let top_side = (n0.z > 0.6 && n1.z.abs() < 0.6) || (n1.z > 0.6 && n0.z.abs() < 0.6);
    if top_side && m.z > H - 0.01 { rounds.push(e); }
}
print(`front deck edges to round: ${rounds.len()}`);
head = head.fillet(rounds, 1.5);

// pockets: cavity in the rear deck, plug pocket at the heel, weight ports from the sole
let cavity = extrude(rounded_rect(vec3(55, 69, 13), 50, 14, 4), vec3(0, 0, 10));
let plug_pocket = extrude(rounded_rect(vec3(25, 25, 10), 30, 20, 4), vec3(0, 0, 20));
let cuts = [cavity, plug_pocket,
    cylinder(vec3(25, 25, -1), vec3(0, 0, 1), 2.25, 12),     // plug screw, M4 clearance
    cylinder(vec3(25, 25, -1), vec3(0, 0, 1), 4.5, 5)];      // Ø9 counterbore from the sole
for p in [[88, 25], [88, 68], [22, 68]] {
    cuts.push(cylinder(vec3(p[0], p[1], -1), vec3(0, 0, 1), 7, 9));       // Ø14 weight port, 8 deep
    cuts.push(cylinder(vec3(p[0], p[1], 7), vec3(0, 0, 1), 2.1, 7));      // M5 tap drill
}
head = head.cut(cuts).unify();

// ---------------------------------------------------------------- plug (in its own coordinates)
let lie = 70.0;  let lean = 2.0;
let axis = rotation(vec3(1, 0, 0), lean) * (rotation(vec3(0, 1, 0), -(90.0 - lie)) * vec3(0, 0, 1));
let base = vec3(6, 0, 10);
let blk = extrude(rounded_rect(vec3(0, 0, 0), 30, 20, 4), vec3(0, 0, 15));
let boss = cylinder(base, axis, 9, 20);
let bore = cylinder(base + axis * 6, axis, 9.55 / 2, 30);
let core = extrude(rounded_rect(vec3(0, 0, 1.5), 27, 17, 2.5), vec3(0, 0, 12));
let plug = fuse([blk, boss]).cut([bore, core]).unify();
// trim the boss flush with the plug's underside
plug = plug.cut(block(vec3(-40, -40, -20), vec3(40, 40, 0)));

// ---------------------------------------------------------------- results
let al6061 = 2.70e-3;  let al7075 = 2.81e-3;  let foam = 0.24e-3;   // g/mm³
let mh = mass_props(head, al6061);
let mp = mass_props(plug, al7075);
let mc = mass_props(core, foam);
let placed = vec3(25, 25, 10);
let total = mh + mp.translate(placed) + mc.translate(placed);

export_step(head, out + "Head.step");
export_step(plug, out + "Plug_70_2.step");
export_step(core, out + "PlugCore_70_2.step");
export_step([head, plug.translate(placed), core.translate(placed)], out + "HeadAssembly.step");
render([#{shape: head, color: "#9aa9ba"}, #{shape: plug.translate(placed), color: "#d9a441"}], #{path: out + "head.png", width: 900, height: 600});

#{
    head: #{volume: head.volume(), mass_g: mh.mass, cg: mh.cg, valid: head.is_valid(), solids: head.solid_count()},
    plug: #{mass_g: mp.mass, cg: mp.cg, valid: plug.is_valid()},
    core: #{mass_g: mc.mass},
    assembly: #{mass_g: total.mass, cg: total.cg, inertia_about_cg: total.inertia}
}
