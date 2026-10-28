// ZeroTorque's 2D region-helper tests, reproduced with the scripting API:
//   cad-rust-app script examples/zerotorque/regions.rhai
// Each check prints PASS or FAIL; the script fails (exit code 1) if any fails.

let failed = 0;
fn check(name, ok) {
    print(`${if ok { "PASS" } else { "FAIL" }}  ${name}`);
    ok
}
let total_area = |faces| { let a = 0.0; for f in faces { a += f.area(); } a };

// 1. four separate discs stay four faces, all facing up
let discs = [];
for c in [[0, 0], [30, 0], [0, 30], [30, 30]] { discs.push(circle(vec3(c[0], c[1], 0), 10)); }
let u = region_union(discs);
let up = true;
for f in u { if f.faces()[0].normal().z < 0.999 { up = false; } }
if !check("four separate discs stay four faces, all facing up", u.len() == 4 && up) { failed += 1; }

// 2. offsetting a region out and back in gives the same area
let region = region_cut(rounded_rect(vec3(0, 0, 0), 60, 40, 6), [circle(vec3(-10, 0, 0), 6), circle(vec3(14, 4, 0), 5)]);
let a0 = total_area.call(region);
let back = [];
for f in region { for g in offset2d(f, 2.0) { for h in offset2d(g, -2.0) { back.push(h); } } }
let a1 = total_area.call(back);
if !check(`offset out and back in keeps the area (${a0} vs ${a1})`, (a1 - a0).abs() / a0 < 1e-6) { failed += 1; }

// 3. two discs closer than twice the radius bridge into one face
let pair = region_union([circle(vec3(0, 0, 0), 10), circle(vec3(19, 0, 0), 10)]);
let apart = region_union([circle(vec3(0, 0, 0), 10), circle(vec3(21, 0, 0), 10)]);
if !check("discs closer than 2r bridge into one face, farther apart stay two", pair.len() == 1 && apart.len() == 2) { failed += 1; }

// 4. the convex hull of equal discs: hull of the centres grown by r, area A + P r + π r²
let r = 5.0;
let centres = [vec3(0, 0, 0), vec3(40, 0, 0), vec3(40, 25, 0), vec3(10, 35, 0), vec3(20, 10, 0)];
let hull = convex_hull(centres);
let poly = polygon(hull);
let grown = offset2d(poly, r);
let perim = 0.0;
for i in 0..hull.len() { perim += distance(hull[i], hull[(i + 1) % hull.len()]); }
let expect = poly.area() + perim * r + PI() * r * r;
let got = total_area.call(grown);
if !check(`convex hull of equal discs (${got} vs ${expect})`, grown.len() == 1 && (got - expect).abs() / expect < 1e-9) { failed += 1; }

if failed > 0 { throw `${failed} region checks failed`; }
"all region checks passed"
