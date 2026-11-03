// Helpers for project.rhai: planes, sketches and extrudes along the world axes.
// Sketch coordinates: on a z plane (u, v) = (x, y); on a y plane (u, v) = (x, z); on an x plane (u, v) = (y, z).

// a construction plane at x, y or z = pos (the origin planes when pos = 0)
fn plane(d, axis, pos) {
    let base = if axis == "z" { "Top" } else if axis == "y" { "Front" } else { "Right" };
    if pos == 0.0 { return base.to_lower(); }
    // the Front plane's normal is -Y, so its offsets run towards -Y
    let off = if axis == "y" { -pos } else { pos };
    d.feature("plane", #{mode: "offset", base: #{kind: "origin_plane", plane: base}, offset: off}, axis + " = " + pos)
}

fn sketch_at(d, axis, pos, name) {
    let p = plane(d, axis, pos);
    d.sketch(p, name)
}

// extrude a sketch (or a list of region refs) drawn on the `axis` plane at a, to b (either side);
// op "add", "remove", "new", "intersect"
fn ext(d, s, axis, a, b, op, name) { ext_expr(d, s, axis, (b - a).abs(), b > a, op, name) }

// the same with a depth expression (variables allowed): towards + when up is true
fn ext_expr(d, s, axis, depth, up, op, name) {
    // Top and Right planes extrude towards +, the Front plane towards -Y
    let opp = if axis == "y" { up } else { !up };
    d.feature("extrude", #{profiles: if type_of(s) == "array" { s } else { [s] }, depth: depth, op: op, opposite: opp}, name)
}

// symmetric about the sketch plane
fn ext_sym(d, s, depth, op, name) {
    d.feature("extrude", #{profiles: if type_of(s) == "array" { s } else { [s] }, depth: depth, op: op, end: "symmetric"}, name)
}

// a full revolve about a world axis through the origin
fn rev(d, s, axis, op, name) {
    let ax = if axis == "x" { 0 } else if axis == "y" { 1 } else { 2 };
    d.feature("revolve", #{profiles: if type_of(s) == "array" { s } else { [s] }, axis: #{kind: "axis", axis: ax}, angle: 360, mode: "full", op: op}, name)
}

// a sketch region picked by a point in it
fn region(s, u, v) { #{kind: "region", sketch: s.id, point: [u, v]} }

// a circular pattern of features about a world axis through the origin ("x", "y", "z")
fn cpattern(d, feats, axis, count, name) {
    let fs = [];
    for f in feats { fs.push(#{kind: "feature", feature: f}); }
    let ax = if axis == "x" { 0 } else if axis == "y" { 1 } else { 2 };
    d.feature("circularPattern", #{kind: "feature", bodies: fs, axis: #{kind: "axis", axis: ax}, count: count}, name)
}

// mirror features about an origin plane ("Front" = the XZ plane, "Right" = YZ, "Top" = XY)
fn mirror(d, feats, plane, name) {
    let fs = [];
    for f in feats { fs.push(#{kind: "feature", feature: f}); }
    d.feature("mirror", #{kind: "feature", bodies: fs, plane: #{kind: "origin_plane", plane: plane}}, name)
}

