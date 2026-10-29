# Scripting cad-app

`cad-app script file.rhai [args…]` runs a [Rhai](https://rhai.rs) script with no window, using the
same OpenCascade kernel as the editor. Arguments are in the `ARGS` array, `print` writes to stdout,
and the script's last value is printed as JSON. A failed operation throws an error with OCCT's
message, which `try { … } catch (e) { … }` can catch; an uncaught error exits with code 1. Fillets,
chamfers, shells and booleans also throw when the result is not a valid solid or has no volume (an
inside-out result is turned right way out). Results
are deterministic: the same script gives the same numbers on every run.

Custom features (`define_custom_feature`) get the same API.

Units are millimetres and degrees. Vectors can be `vec3(x, y, z)` values or `[x, y, z]` arrays, and
`[x, y]` means z = 0.

```rhai
let body = extrude(rounded_rect(vec3(0, 0, 0), 60, 40, 5), vec3(0, 0, 12));
// round every edge between the top face and a side face, in one fillet operation
let picks = [];
for e in body.edges() {
    let f = e.faces();
    let m = e.mid();
    if f.len() == 2 && (f[0].normal(m).z > 0.6) != (f[1].normal(m).z > 0.6) && m.z > 11.9 { picks.push(e); }
}
body = body.fillet(picks, 2);
let mp = mass_props(body, 2.7e-3);           // density in g/mm³: mass in g
export_step(body, "body.step");
#{mass: mp.mass, cg: mp.cg, inertia: mp.inertia}
```

`examples/zerotorque/parts.rhai` builds a putter head, a hosel plug and its foam core this way, and
`examples/zerotorque/regions.rhai` runs the 2D region checks.

## Shapes

| function | result |
|---|---|
| `block(lo, hi)` | box between two corners |
| `cylinder(base, axis, r, h)`, `cone(base, axis, r1, r2, h)`, `sphere_at(c, r)` | primitives |
| `cube(w, d, h)`, `cylinder(r, h)`, `sphere(r)`, `torus(R, r)`, `prism(pts2, h)`, `revolve(pts_rz, deg)` | primitives at the origin |
| `wire(points, closed)`, `arc(a, b, c)`, `join([wires or edges])` | wires |
| `polygon(points)`, `circle(c, r)`, `circle(c, normal, r)`, `rounded_rect(center, w, h, r)`, `face(wire)`, `face(outer, [holes])` | planar faces |
| `offset2d(face, d)`, `offset2d(face, d, arcs)` | 2D offset (OCCT MakeOffset): out for d > 0, in for d < 0, arc joins by default; an array of faces |
| `region_union(faces)`, `region_union(a, b)`, `region_cut(a, b)`, `region_common(a, b)` | 2D region booleans: one face per connected piece, all facing the way the first face does |
| `extrude(face, v)`, `loft(profiles, ruled)`, `revolve(face, origin, axis, deg)` | solids from profiles (loft takes wires or faces) |
| `fuse(shapes)`, `fuse(shapes, fuzzy)`, `s.cut(tools)`, `s.cut(tools, fuzzy)`, `s.common(tools[, fuzzy])` | booleans with many tools at once, run in parallel; `fuzzy` (mm) treats gaps smaller than it as touching |
| `a + b`, `a - b`, `a & b` | two-shape booleans (these also merge coplanar faces) |
| `s.unify()` | remove splitter: merge coplanar faces and collinear edges |
| `s.translate(v)`, `s.rotate(point, axis, deg)`, `s.mirror(point, normal)`, `s.transform(mat3, t)`, `s.copy()` | placement |
| `s.move(x, y, z)`, `s.rotate_x/y/z(deg)`, `s.place(point, normal)`, `s.scale(f)` | placement about the origin |
| `compound(shapes)`, `s.solids()` | group shapes, split into solids |
| `s.shell(faces, t)` | hollow out, opening the given faces (`[]`: a closed hollow with a cavity inside); walls `t` thick inside the outer surface, or for negative `t` grown outside it (up to version 0.4.0 the sign was the other way round) |
| `s.fillet(edges, r)`, `s.fillet(edges, r1, r2)`, `s.chamfer(edges, d)` | fillet or chamfer chosen edges in one operation |
| `s.fillet(r)`, `s.chamfer(d)` | every edge |

## Faces and edges

`s.faces()` and `s.edges()` return Face and Edge values that know their neighbours:

| | |
|---|---|
| `e.faces()`, `f.edges()` | faces at an edge, edges of a face |
| `f.normal(p)`, `f.normal()` | outward normal at the point of the face nearest `p` (or at its centre) |
| `f.kind()`, `e.kind()` | `plane`, `cylinder`, `cone`, `sphere`, `torus`, `bspline`, …; `line`, `circle`, `ellipse`, `bspline`, … |
| `f.center()`, `f.area()`, `f.closest(p)`, `f.axis()` | `axis`: `#{point, axis, radius}` for planes, cylinders, cones, spheres and tori |
| `e.start()`, `e.mid()`, `e.end()`, `e.length()`, `e.circle()`, `e.points(deflection)` | `circle`: `#{center, axis, radius}` or `()` |
| `same(a, b)`, `a == b` | the same face or edge (OCCT IsSame) |
| `f.shape()`, `e.shape()` | as a Shape |

## Queries

| | |
|---|---|
| `s.volume()`, `s.area()`, `s.length()`, `s.centroid()`, `s.bbox()` | `bbox`: `#{min, max, size, center}` |
| `mass_props(s, density)` | a MassProps: `.mass`, `.cg`, `.inertia` (3 × 3, about the CG) |
| `s.is_valid()`, `s.solid_count()`, `s.face_count()`, `s.edge_count()`, `s.is_empty()` | |
| `s.classify(points, tol)`, `s.classify(p, tol)`, `s.contains(p)` | `"in"`, `"out"` or `"on"` for each point, classified on several threads |
| `s.tessellate(linear, angular_deg)` | `#{vertices, indices, faces}` (flat arrays) |
| `s.hash()` | a fingerprint of the geometry, the same for the same shape on every run, for caching |
| `s.ray_hits(origins, dir)`, `s.ray_cast(origins, dir)` | crossing counts, or `[#{distance, point}]` per ray |
| `accessibility(s, directions)` | the machinability check: `#{total_area, unreachable_area, worst: [#{point, normal, area}]}`; directions point from the tool into the part, so `[[0, 0, -1], [0, 0, 1]]` means from above and from below |

## Mass properties

`mass_props(shape, density)`, `mass_props(mass, cg, inertia)`, `point_mass(mass, at)`; `a + b` and
`combine([…])` add them (parallel axis theorem); `m.translate(v)`, `m.rotate(center, axis, deg)`,
`m.scale_mass(k)`, `m.inertia_about(point)` (a matrix), `m.inertia_about(point, dir)` (about a line),
`m.to_map()`.

Balance helpers: `line_distance(p, a, dir)` is the distance from `p` to the line through `a` along
`dir`; `line_offset(p, a, dir)` the perpendicular offset vector, and `line_offset(p, a, dir, u, v)`
its components along `u` and `v`.

## Numbers

| | |
|---|---|
| `vec3`, `+ - * /`, `dot`, `cross`, `norm`, `normalize`, `distance`, `.x .y .z` | vectors |
| `mat3(rows)`, `identity()`, `rotation(axis, deg)`, `outer(a, b)`, `m * v`, `m * m`, `m.transpose()`, `m.inverse()`, `m.det()`, `m.get(i, j)`, `m.rows()` | 3 × 3 matrices |
| `zeros(n)`, `linspace(a, b, n)`, `sum`, `mean`, `min_of`, `max_of`, `argmin`, `argmax`, `argsort`, `cumsum` | arrays of numbers |
| `add`, `sub`, `mul`, `div` (array or number either side), `abs_all`, `lt`, `gt`, `take(a, indices)`, `select(a, mask)` | elementwise |
| `column(rows, j)`, `transpose(rows)` | 2D arrays (arrays of rows) |
| `product([list, list, …])` | every combination taking one element from each list (5 materials in 4 ports: 625) |
| `convex_hull(points)` | convex hull in the XY plane, counter-clockwise |
| `rng(seed)`, `r.uniform()`, `r.uniform(lo, hi)`, `r.int(n)`, `r.points_in_box(lo, hi, n)` | seeded random numbers |

## Files and output

`read_json(path)`, `read_yaml(path)`, `parse_json(text)`, `parse_yaml(text)`, `write_json(path, value)`,
`read_text`, `write_text`, `to_json(value)`; `import_step(path)`; `export_step(shape or [shapes], path)`
(an array becomes one file with a body per shape), `export_stl(shape or [shapes], path[, deflection])`.

`render(parts, options)` writes a PNG with smooth shading and no edges. Parts are shapes or
`#{shape, color, opacity}` maps (`color` `"#rrggbb"` or `[r, g, b]`; `opacity` below 1 for X-ray
views). Options: `path`, `width`, `height`, `view` (`iso`, `top`, `front`, …) or `eye`, `target`,
`up`, `fov` (degrees; 0 or none: orthographic), `background`. `render_views(views, options)` puts
several viewports in one image: each view is a map with its own `parts` and camera, laid out in
`columns`. For tubes and capsules build cylinders and `sphere_at`s.
