// Fit ladder coupon for the parts on hand: 608 and 625 bearings, 3/4/5 mm dowels, NEMA17 D-shaft.
// Five columns, each one step looser (offsets on the DIAMETER). The first plate printed outer
// contours 0.4 mm small (96 x 46 measured 95.6 x 45.6), so: holes nominal + 0.2 .. + 0.6 and
// posts nominal + 0.2 .. - 0.2. The column number is the count of dots on the dowel block.
let out = if ARGS.len() > 0 { ARGS[0] + "/" } else { "" };
fn zcyl(x, y, z0, r, h) { cylinder(vec3(x, y, z0), vec3(0, 0, 1), r, h) }
let T = 2.0;
let W = 136.0;  let L = 148.0;
let adds = [];  let cuts = [];
for i in 0..5 {
    let k = 0.2 + 0.1 * i;            // hole offset
    let p = 0.2 - 0.1 * i;            // post offset
    let x = 14.0 + 27.0 * i;
    // A: 608 outer race Ø22 into a bore (press)   (also the NEMA17 Ø22 pilot boss)
    adds.push(zcyl(x, 15, 0, 11.0 + k / 2.0 + 2.5, T + 7.0));
    cuts.push(zcyl(x, 15, -1, 11.0 + k / 2.0, T + 9.0));
    // B: 608 inner race Ø8 onto a post
    adds.push(zcyl(x, 43, 0, 4.0 + p / 2.0, T + 7.0));
    // C: 625 outer race Ø16 into a bore
    adds.push(zcyl(x, 69, 0, 8.0 + k / 2.0 + 2.5, T + 5.0));
    cuts.push(zcyl(x, 69, -1, 8.0 + k / 2.0, T + 7.0));
    // D: 625 inner race Ø5 onto a post
    adds.push(zcyl(x, 91, 0, 2.5 + p / 2.0, T + 5.0));
    // E: Ø3 / Ø4 / Ø5 dowel holes in an 8 mm block
    adds.push(block(vec3(x - 9, 103, 0), vec3(x + 9, 133, 8)));
    cuts.push(zcyl(x, 109, -1, 1.5 + k / 2.0, 10));
    cuts.push(zcyl(x, 117, -1, 2.0 + k / 2.0, 10));
    cuts.push(zcyl(x, 125.5, -1, 2.5 + k / 2.0, 10));
    for j in 0..(i + 1) { cuts.push(zcyl(x - 5.0 + 2.5 * j, 130.5, 7.4, 0.8, 1)); }
    // F: NEMA17 Ø5 D-shaft (flat 0.5 deep, on +X), in a Ø12 x 10 boss
    adds.push(zcyl(x, 140, 0, 6.0, 10));
    cuts.push(zcyl(x, 140, -1, 2.5 + k / 2.0, 12).cut([block(vec3(x + 2.0 + k / 2.0, 136, -2), vec3(x + 4, 144, 12))]));
}
// one chamfered corner marks column 1 / the 608 row
let base = block(vec3(0, 0, 0), vec3(W, L, T)).cut([block(vec3(-1, -1, -1), vec3(6, 6, 4)).rotate(vec3(0, 0, 0), vec3(0, 0, 1), 0.0)]);
let shape = fuse([base] + adds).unify().cut(cuts);
export_stl(shape, out + "print-Fit-ladder.stl", 0.03);
export_step(shape, out + "printed-Fit-ladder.step");
render([#{shape: shape, color: "#d6a83a"}], #{path: out + "fit-ladder.png", width: 1000, height: 900, eye: [200, -150, 260], target: [68, 74, 0], up: [0, 0, 1], fov: 35, background: "#f3f4f6"});
write_json(out + "volume.json", #{volume: shape.volume()});
