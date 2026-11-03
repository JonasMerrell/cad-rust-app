// rcad script check_project.rhai native.step reference.step pitch [dx dy dz] [left|right]
// Samples a grid (pitch mm) over a part from project.rhai (moved by dx dy dz, or one solid of it) and
// its parts.rhai reference (left / right: the solid at +Y / -Y of a split part), and reports the volume inside only one of them. (A point test: rCAD's
// booleans between two copies of a part give nothing, the faces coincide.)
let a = import_step(ARGS[0]);
if ARGS.len() > 6 {
    for s in a.solids() { if (s.bbox().center.y > 0.0) == (ARGS[6] == "left") { a = s; } }
}
if ARGS.len() > 5 { a = a.translate(vec3(parse_float(ARGS[3]), parse_float(ARGS[4]), parse_float(ARGS[5]))); }
let b = import_step(ARGS[1]);
let h = parse_float(ARGS[2]);
let ba = a.bbox(); let bb = b.bbox();
let lo = [ba.min.x.min(bb.min.x), ba.min.y.min(bb.min.y), ba.min.z.min(bb.min.z)];
let hi = [ba.max.x.max(bb.max.x), ba.max.y.max(bb.max.y), ba.max.z.max(bb.max.z)];
let extra = []; let missing = [];
let x = lo[0] + 0.0731 * h;
while x < hi[0] {
    let pts = [];
    let y = lo[1] + 0.417 * h;
    while y < hi[1] { let z = lo[2] + 0.593 * h; while z < hi[2] { pts.push([x, y, z]); z += h; } y += h; }
    let ca = a.classify(pts, 1e-6); let cb = b.classify(pts, 1e-6);
    for i in 0..pts.len() {
        let ia = ca[i] == "in"; let ib = cb[i] == "in";
        if ia && !ib { extra.push(pts[i]); } else if ib && !ia { missing.push(pts[i]); }
    }
    x += h;
}
fn box(ps) {
    if ps.len() == 0 { return (); }
    let lo = [1e9, 1e9, 1e9]; let hi = [-1e9, -1e9, -1e9];
    for p in ps { for k in 0..3 { lo[k] = lo[k].min(p[k]); hi[k] = hi[k].max(p[k]); } }
    [lo, hi]
}
#{native_mm3: a.volume(), reference_mm3: b.volume(), only_native_mm3: extra.len() * h * h * h, only_reference_mm3: missing.len() * h * h * h,
  only_native_box: box(extra), only_reference_box: box(missing)}
