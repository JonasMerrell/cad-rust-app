"""DeWalt DC742 drill v2: native rCAD Part Studio fitted to the drill scan, with the photo details.

Frame: the scan_photos result frame, in mm. The drill stands on XY, Z is up and the chuck points to -X.
The head (chuck, clutch, housing, rear cap and their details) is built on a local axis along +X through
the origin. It is then rotated about Y by TILT and moved onto the measured axis (Y0, Z0 at x = 0).
The grip and the battery are lofts through section outlines measured from the scan points (sections.json).

    python3 gen2.py run2.json [prefix]   ->  rcad run run2.json
"""
import json, math, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

HERE = os.path.dirname(os.path.abspath(__file__))
SEC = json.load(open(os.path.join(HERE, "sections.json")))
TXT = json.load(open(os.path.join(HERE, "text_counts.json")))

TILT = -5.71          # deg: head axis rises 0.1 mm per mm towards the rear (fitted)
Y0, Z0 = 22.5, 203.9  # head axis at x = 0 (fitted)
YC = 23.7             # grip / trigger mid-plane (fitted)

YEL, BLK, SIL, DRK, GRY, WHT = "#f2b705", "#1c1c1c", "#b9b9b9", "#3a3a3a", "#5a5a5a", "#e8e8e8"

steps = [{"tool": "new_document", "args": {}}]
nid = [0]
PARTS = []      # (name, feature id, body index, colour)
HEAD = []       # (feature id, body index) to move with the head


def new_id():
    nid[0] += 1
    return nid[0]


def plane(base, offset, flip=False):
    i = new_id()
    p = {"mode": "offset", "base": {"kind": "origin_plane", "plane": base}, "offset": str(round(offset, 4))}
    if flip:
        p["flip"] = True
    steps.append({"tool": "add_feature", "args": {"kind": "plane", "name": f"Plane {base} {offset:.2f}", "params": p}})
    return i


def sketch(pl, items, name):
    i = new_id()
    p = pl if isinstance(pl, str) else {"kind": "feature_plane", "feature": pl}
    steps.append({"tool": "create_sketch", "args": {"plane": p, "name": name}})
    steps.append({"tool": "sketch_add", "args": {"sketch_id": i, "items": items}})
    return i


def feat(kind, params, name):
    i = new_id()
    steps.append({"tool": "add_feature", "args": {"kind": kind, "params": params, "name": name}})
    return i


def body(f, i=0):
    return {"kind": "body", "body": [f, i]}


def part(name, f, colour, head=False, count=1):
    for i in range(count):
        PARTS.append((name if count == 1 else f"{name} {i + 1}", f, i, colour))
        if head:
            HEAD.append((f, i))


def ext(pl, items, name, depth, end=None, op="new"):
    s = sketch(pl, items, name + " sketch")
    p = {"profiles": [{"sketch": s}], "depth": depth, "op": op}
    if end:
        p["end"] = end
    return feat("extrude", p, name)


def union(target, tools, name):
    if tools:
        feat("boolean", {"kind": "union", "target": body(*target), "tools": [body(*t) for t in tools]}, name)


from blockfont import text_items


def text_body(pl, key, txt, at, h, depth, name, colour, head=False, end=None):
    """Raised lettering from block-stroke glyphs (rCAD sketch text drops overlapping font contours)."""
    items, n = text_items(txt, at[0], at[1], h)
    f = ext(pl, items, name, depth, end=end)
    part(name, f, colour, head, count=n)
    return f


AXIS_CURVE = lambda n: 2 * n + 3   # a closed polyline of n points, then one construction line


def revolve(profile, name, colour, op="new"):
    pts = [[x, r] for x, r in profile]
    s = sketch("front", [{"type": "polyline", "closed": True, "points": pts},
                         {"type": "line", "from": [-90, 0], "to": [170, 0], "construction": True}], name + " profile")
    f = feat("revolve", {"profiles": [{"sketch": s}], "axis": {"kind": "sketch_curve", "sketch": s, "curve": AXIS_CURVE(len(pts))}, "op": op}, name)
    if colour:
        part(name, f, colour, head=True)
    return f


# ======================= head (local frame: axis = X, y = 0, z = 0) =======================
HOUSING = [(10, 27.5), (14, 30.0), (40, 31.3), (70, 31.8), (100, 31.2), (112, 30.4)]
REAR = [(112, 30.4), (122, 28.8), (128, 26.5), (133, 23), (138, 18.5), (143, 13), (147, 7), (149.5, 0)]


def r_at(x):
    prof = HOUSING + REAR[1:]
    for (x0, r0), (x1, r1) in zip(prof, prof[1:]):
        if x0 <= x <= x1:
            return r0 + (r1 - r0) * (x - x0) / (x1 - x0)
    return 0.0


f_nose = revolve([(-61, 0), (-61, 5.2), (-60, 6.6), (-58, 7.6), (-55, 9.5), (-53, 11), (-53, 0)], "Chuck nose", DRK)
f_fs = revolve([(-53, 0), (-53, 15), (-50, 18.2), (-47, 19.8), (-26, 19.8), (-24.5, 18.5), (-24, 0)], "Chuck front sleeve", BLK)
f_rs = revolve([(-24, 0), (-24, 19.3), (-22, 20.5), (-9, 20.5), (-7, 19.3), (-7, 0)], "Chuck rear sleeve", BLK)
revolve([(-7, 0), (-7, 17.8), (-2, 17.8), (-2, 0)], "Chuck ring (silver)", SIL)
revolve([(-2, 0), (-2, 21), (-1, 21.9), (5, 25.3), (6, 25.0), (10, 27.5), (10, 0)], "Clutch collar", BLK)
f_h = revolve([(10, 0)] + HOUSING + [(112, 0)], "Housing", YEL)
f_r = revolve([(112, 0)] + REAR, "Rear cap", BLK)

# chuck jaw opening and the three jaw tips
ext(plane("Right", -61), [{"type": "circle", "center": [0, 0], "radius": 4.8}], "Chuck jaw opening", 26, end="symmetric", op="remove")
fj = ext(plane("Top", 1.6), [{"type": "rect", "corner1": [-60.5, -1.1], "corner2": [-52, 1.1]}], "Chuck jaw", 2.6)
pj = feat("circularPattern", {"kind": "part", "bodies": [body(fj)], "axis": {"kind": "axis", "axis": 0}, "count": "3", "angle": "360"}, "Chuck jaws x3")
part("Chuck jaw", fj, SIL, head=True)
part("Chuck jaw", pj, SIL, head=True, count=2)


# chuck grip ribs: a groove tool patterned round the axis, then subtracted
def groove_ring(x0, x1, r, depth, width, count, target, name):
    s = sketch(plane("Top", r - depth), [{"type": "rect", "corner1": [x0, -width / 2], "corner2": [x1, width / 2]}], name + " sketch")
    tool = feat("extrude", {"profiles": [{"sketch": s}], "depth": depth + 3, "op": "new"}, name + " tool")
    pat = feat("circularPattern", {"kind": "part", "bodies": [body(tool)], "axis": {"kind": "axis", "axis": 0}, "count": str(count), "angle": "360"}, name + " pattern")
    feat("boolean", {"kind": "subtract", "target": body(target), "tools": [body(tool)] + [body(pat, i) for i in range(count - 1)]}, name)


groove_ring(-47, -27, 19.8, 1.2, 2.2, 16, f_fs, "Chuck front sleeve grip ribs")
groove_ring(-21, -10, 20.5, 1.0, 2.0, 24, f_rs, "Chuck rear sleeve grip ribs")

# clutch numbers: raised along both sides of the collar cone (3,5,7,9 on +Y; 1,17,15,13 on -Y)
for side, nums in ((1, ("3", "5", "7", "9")), (-1, ("1", "17", "15", "13"))):
    for k, n in enumerate(nums):
        zc = 7.5 - 5.2 * k
        rr = 24.0
        ysurf = math.sqrt(max(rr * rr - zc * zc, 1)) - 0.6
        if side > 0:
            pl = plane("Front", -ysurf, flip=True)     # normal +Y, sketch x = -X
            at = [-5.2, zc - 1.6]
        else:
            pl = plane("Front", ysurf)                 # normal -Y, sketch x = +X
            at = [1.6, zc - 1.6]
        text_body(pl, "c" + n, n, at, 3.2, 1.2, f"Clutch number {n}", YEL, head=True)

# parting lines: grooves where the two shell halves meet (mid-plane y = 0), top and bottom
for sgn, nm in ((1, "top"), (-1, "bottom")):
    xs = [12 + 4 * i for i in range(34)] + [146.5]
    outer = [[x, sgn * (r_at(x) + 2.5)] for x in xs]
    inner = [[x, sgn * (r_at(x) - 0.5)] for x in reversed(xs)]
    ext("front", [{"type": "polyline", "closed": True, "points": outer + inner}], f"Parting line groove ({nm})", 0.8, end="symmetric", op="remove")

# speed selector: raised yellow frame, pocket, black slider, "1" / "2" markings
fr = ext(plane("Top", 26), [{"type": "rect", "corner1": [24, -11.5], "corner2": [68, 11.5]}], "Speed selector frame", 9)
union((f_h, 0), [(fr, 0)], "Speed selector frame onto housing")
ext(plane("Top", 35), [{"type": "rect", "corner1": [30, -7], "corner2": [62, 7]}], "Speed selector pocket", 10, end="symmetric", op="remove")
fs = ext(plane("Top", 29), [{"type": "rect", "corner1": [42, -5.5], "corner2": [50, 5.5]}, ], "Speed selector slider", 6.5)
part("Speed selector slider", fs, BLK, head=True)
ext(plane("Top", 33.5), [{"type": "rect", "corner1": [44, -5.5], "corner2": [48, 5.5]}], "Slider grip ridge cut", 2, end="symmetric", op="remove")
text_body(plane("Top", 35), "s1", "1", [25.6, -2.0], 4, 0.5, "Speed mark 1", BLK, head=True)
text_body(plane("Top", 35), "s2", "2", [63.6, -2.0], 4, 0.5, "Speed mark 2", BLK, head=True)

# raised screw bosses on the upper shoulders beside the speed selector (yellow, scan: ~8 mm proud at 45 deg)
BOSS = [(20, 16.5), (20, -16.5), (66, 16.5), (66, -16.5)]
fbo = ext(plane("Top", 18), [{"type": "circle", "center": [x, y], "radius": 4.6} for x, y in BOSS], "Shoulder screw bosses", 15.0)
union((f_h, 0), [(fbo, i) for i in range(4)], "Shoulder screw bosses onto housing")
# black screw caps: on the bosses, and on the rear shoulders
fc = ext(plane("Top", 32.6), [{"type": "circle", "center": [x, y], "radius": 2.6} for x, y in BOSS], "Boss screw caps", 1.0)
part("Boss screw cap", fc, BLK, head=True, count=4)
fc2 = ext(plane("Top", 22), [{"type": "circle", "center": [104, 15.5], "radius": 2.6}, {"type": "circle", "center": [104, -15.5], "radius": 2.6}], "Rear screw caps", 6.0)
part("Rear screw cap", fc2, BLK, head=True, count=2)

# screw holes on the -Y shell half (rear cap bosses seen in IMG_0001), counterbored
for k, (x, z) in enumerate(((116, 17), (139, 4), (24, -20), (96, -22))):
    ys = math.sqrt(max(r_at(x) ** 2 - z * z, 1))
    ext(plane("Front", ys), [{"type": "circle", "center": [x, z], "radius": 2.7}], f"Screw counterbore {k + 1} (-Y)", 4, end="symmetric", op="remove")
    ext(plane("Front", ys - 2), [{"type": "circle", "center": [x, z], "radius": 1.2}], f"Screw hole {k + 1} (-Y)", 6, end="symmetric", op="remove")

# rear vent grilles: yellow oval inserts in the black rear cap, then 5 slots through both
for side in (1, -1):
    pl = plane("Front", -side * 24.6)
    fg = ext(pl, [{"type": "ellipse", "center": [124, -1], "major_end": [124, 12], "minor_radius": 9}], f"Vent grille insert {'+Y' if side > 0 else '-Y'}", 12, end="symmetric")
    skin = revolve([(112, 0)] + [(x, r + 0.35) for x, r in REAR[:-1]] + [(149.8, 0)], f"Grille skin tool {'+Y' if side > 0 else '-Y'}", None)
    feat("boolean", {"kind": "intersect", "target": body(fg), "tools": [body(skin)]}, f"Vent grille insert {'+Y' if side > 0 else '-Y'} to the cap surface")
    part(f"Vent grille insert {'+Y' if side > 0 else '-Y'}", fg, YEL, head=True)
    items = [{"type": "rect", "corner1": [118, z], "corner2": [131, z + 2.0]} for z in (-10.5, -6.3, -2.1, 2.1, 6.3)]
    ext(plane("Front", -side * 31.5), items, f"Side vents {'+Y' if side > 0 else '-Y'}", 15, end="symmetric", op="remove")

# rear-top vent slots ("E" vent)
ext(plane("Top", 27), [{"type": "rect", "corner1": [117, y], "corner2": [126, y + 2.2]} for y in (-9.3, -5.1, -0.9, 3.3, 7.5)], "Rear top vents", 8, end="symmetric", op="remove")

# flat label panels with plates and lettering: DEWALT (-Y), DC742 rating label (+Y)
ext(plane("Front", 33.5), [{"type": "rect", "corner1": [32, -12], "corner2": [88, 6]}], "Logo panel recess (-Y)", 10, end="symmetric", op="remove")
ext(plane("Front", -33.5), [{"type": "rect", "corner1": [22, -11], "corner2": [92, 7]}], "Label panel recess (+Y)", 10, end="symmetric", op="remove")
fl = ext(plane("Front", 28.75), [{"type": "rect", "corner1": [34, -10.5], "corner2": [86, 4.5]}], "DEWALT logo plate (-Y)", 0.7, end="symmetric")
part("DEWALT logo plate (-Y)", fl, BLK, head=True)
fl2 = ext(plane("Front", -28.75), [{"type": "rect", "corner1": [24, -9.5], "corner2": [90, 5.5]}], "DC742 label plate (+Y)", 0.7, end="symmetric")
part("DC742 label plate (+Y)", fl2, BLK, head=True)
text_body(plane("Front", 29.1), "dewalt", "DEWALT", [38.5, -7.3], 8.5, 0.5, "DEWALT lettering", YEL, head=True)
text_body(plane("Front", -29.1, flip=True), "dc742", "DC742", [-88, -0.6], 5.0, 0.4, "DC742 lettering", YEL, head=True)

HEAD_REFS = [body(f, i) for f, i in HEAD]
feat("transform", {"mode": "rotate", "bodies": HEAD_REFS, "axis": {"kind": "axis", "axis": 1}, "angle": str(TILT)}, "Tilt head to the scanned axis")
feat("transform", {"mode": "translate", "bodies": HEAD_REFS, "direction": {"kind": "axis", "axis": 1}, "distance": str(Y0)}, "Head to Y")
feat("transform", {"mode": "translate", "bodies": HEAD_REFS, "direction": {"kind": "axis", "axis": 2}, "distance": str(Z0)}, "Head to Z")


# ======================= grip (world frame) =======================
def spl(pts):
    return [{"type": "spline", "points": pts, "closed": True}]


def offset_pts(pts, d):
    c = [sum(p[0] for p in pts) / len(pts), sum(p[1] for p in pts) / len(pts)]
    out = []
    for p in pts:
        v = [p[0] - c[0], p[1] - c[1]]
        L = math.hypot(*v)
        out.append([round(p[0] + v[0] / L * d, 3), round(p[1] + v[1] / L * d, 3)])
    return out


GZ = [82, 94, 106, 118, 130, 142, 154, 166]
# the lofts end at the measured z 166 section (a section above it makes the loft overshoot); z 182 and up is
# the housing itself, so the top section is carried straight up into the housing by an extrude
gsecs = [(74, SEC["grip82"])] + [(z, SEC[f"grip{z}"]) for z in GZ]
gsk = [sketch(plane("Top", z), spl(p), f"Grip section z{z}") for z, p in gsecs]
fg_core = feat("loft", {"profiles": [{"sketch": s} for s in gsk], "op": "new"}, "Grip core (yellow)")
fg_top = feat("extrude", {"profiles": [{"sketch": gsk[-1]}], "depth": 22, "op": "new"}, "Grip top into the housing")
union((fg_core, 0), [(fg_top, 0)], "Grip top onto the grip")
part("Grip core (yellow)", fg_core, YEL)
osecs = [(86, offset_pts(SEC["grip82"], 1.0))] + [(z, offset_pts(SEC[f"grip{z}"], 1.0)) for z in GZ[1:]]
osk = [sketch(plane("Top", z), spl(p), f"Overmould section z{z}") for z, p in osecs]
fg_over = feat("loft", {"profiles": [{"sketch": s} for s in osk], "op": "new"}, "Grip overmould (black rubber)")
fo_top = feat("extrude", {"profiles": [{"sketch": osk[-1]}], "depth": 10, "op": "new"}, "Overmould top")
union((fg_over, 0), [(fo_top, 0)], "Overmould top onto the overmould")
# the overmould covers the back and sides; the yellow strip at the front stays visible
pl_mid = plane("Front", -YC)
front = [[75.0 + 8.5, 86], [72.4 + 8.5, 94], [68.3 + 8.5, 106], [65.8 + 8.5, 118], [63.4 + 8.0, 128], [64, 132], [64, 182], [170, 182], [170, 86]]
fo_tool = ext(pl_mid, [{"type": "polyline", "closed": True, "points": front}], "Overmould extent tool", 90, end="symmetric")
feat("boolean", {"kind": "intersect", "target": body(fg_over), "tools": [body(fo_tool)]}, "Overmould outline (two-tone)")
part("Grip overmould (black rubber)", fg_over, BLK)

# trigger with texture ribs on the finger face
trig = [[66, 128], [55, 130], [47.5, 134], [42.5, 141], [40.5, 150], [41, 160], [43.5, 167], [48, 171.5], [66, 173]]
ft = ext(pl_mid, [{"type": "polyline", "closed": True, "points": trig}], "Trigger", 15, end="symmetric")
part("Trigger", ft, BLK)
ext(pl_mid, [{"type": "rect", "corner1": [38, z], "corner2": [42.2, z + 1.1]} for z in (143.5, 147.5, 151.5, 155.5, 159.5)], "Trigger texture ribs", 18, end="symmetric", op="remove")

# forward/reverse button: a capsule through the top of the grip, proud on both sides
fb = ext(pl_mid, [{"type": "line", "from": [49, 173.5], "to": [56, 173.5]},
                  {"type": "arc", "center": [56, 177.5], "start": [56, 173.5], "end": [56, 181.5]},
                  {"type": "line", "from": [56, 181.5], "to": [49, 181.5]},
                  {"type": "arc", "center": [49, 177.5], "start": [49, 181.5], "end": [49, 173.5]}], "Forward/reverse button", 47, end="symmetric")
part("Forward/reverse button", fb, BLK)

# ======================= battery (world frame) =======================
def rrect(cx, cy, w, h, r):
    x0, x1, y0, y1 = cx - w / 2, cx + w / 2, cy - h / 2, cy + h / 2
    return [
        {"type": "line", "from": [x0 + r, y0], "to": [x1 - r, y0]},
        {"type": "arc", "center": [x1 - r, y0 + r], "start": [x1 - r, y0], "end": [x1, y0 + r]},
        {"type": "line", "from": [x1, y0 + r], "to": [x1, y1 - r]},
        {"type": "arc", "center": [x1 - r, y1 - r], "start": [x1, y1 - r], "end": [x1 - r, y1]},
        {"type": "line", "from": [x1 - r, y1], "to": [x0 + r, y1]},
        {"type": "arc", "center": [x0 + r, y1 - r], "start": [x0 + r, y1], "end": [x0, y1 - r]},
        {"type": "line", "from": [x0, y1 - r], "to": [x0, y0 + r]},
        {"type": "arc", "center": [x0 + r, y0 + r], "start": [x0, y0 + r], "end": [x0 + r, y0]},
    ]


# rounded-rectangle sections fitted to the scan points per height (least median distance)
RR = {10: 12, 18: 12, 26: 12, 34: 12, 42: 12, 50: 12, 56: 12, 60: 13, 64: 15, 68: 17, 72: 18, 76: 18}


import numpy as _np


def _fitline(keys, deg):
    zs = _np.array(keys, float)
    fits = {}
    for p in ("cx", "cy", "w", "h"):
        v = _np.array([SEC[f"batrr{k}"][p] for k in keys])
        fits[p] = _np.polyfit(zs, v, deg)
    return lambda z: {p: float(_np.polyval(c, z)) for p, c in fits.items()}


# sections smoothed along Z: a straight-line fit for the black base, a quadratic for the yellow top
_black = _fitline([10, 18, 26, 34, 42], 1)
_yellow = _fitline([42, 50, 56, 60, 64, 68, 72, 76], 2)


def bsec(z, key):
    q = _black(z) if z <= 40 else _yellow(z)
    r = 12.5 if z <= 56 else 12.5 + (z - 56) * 0.25
    return rrect(q["cx"], q["cy"], q["w"], q["h"], r)


BZ_BLACK = [(0.0, 0), (10, 10), (20, 20), (30, 30), (40, 40)]
BZ_YEL = [(40, 40), (48, 48), (56, 56), (62, 62), (68, 68), (72, 72), (76, 76)]
fbb = feat("loft", {"profiles": [{"sketch": sketch("top" if z == 0 else plane("Top", z), bsec(z, k), f"Battery section z{z:g}")} for z, k in BZ_BLACK], "op": "new"}, "Battery pack base (black)")
part("Battery pack base (black)", fbb, BLK)
fby = feat("loft", {"profiles": [{"sketch": sketch(plane("Top", z), bsec(z, k), f"Battery section z{z:g} (top)")} for z, k in BZ_YEL], "op": "new"}, "Battery pack top (yellow)")
part("Battery pack top (yellow)", fby, YEL)

# label recess and "12 VOLT" print
ext(plane("Front", 16.8), [{"type": "rect", "corner1": [68, 20.5], "corner2": [112, 35]}], "Battery label recess (-Y)", 1.6, end="symmetric", op="remove")
text_body(plane("Front", 16.0), "b12", "12", [83, 27.5], 6, 0.5, "12 V print", WHT)
text_body(plane("Front", 16.0), "volt", "VOLT", [81, 21.8], 4.0, 0.5, "VOLT print", WHT)

# release buttons on both sides (yellow tab + black ridged latch below)
for side in (1, -1):
    tag = "+Y" if side > 0 else "-Y"
    yface = 67.4 if side > 0 else -17.1
    fbtn = ext(plane("Front", -(yface - side * 1.0)), [{"type": "rect", "corner1": [79, 40], "corner2": [101, 56]}], f"Battery release button {tag}", 7, end="symmetric")
    part(f"Battery release button {tag}", fbtn, YEL)
    flr = ext(plane("Front", -yface), [{"type": "rect", "corner1": [82, z], "corner2": [98, z + 1.2]} for z in (31.5, 34.5, 37.5)], f"Battery latch ridges {tag}", 3, end="symmetric")
    part(f"Battery latch ridge {tag}", flr, BLK, count=3)


def finish(prefix, with_scan=False):
    out = [{"tool": "set_parts", "args": {"parts": [{"body": [f, i], "name": name, "color": col, "finish": "metal" if col == SIL else "satin"} for name, f, i, col in PARTS]}}]
    out.append({"tool": "export", "args": {"path": f"{prefix}.stl", "format": "stl"}})
    out.append({"tool": "export", "args": {"path": f"{prefix}.step", "format": "step"}})
    for v in ("iso_front_left", "iso", "iso_back_right", "left", "front", "back", "top"):
        out.append({"tool": "render", "args": {"path": f"{prefix}-{v}.png", "view": v, "width": 2400, "height": 1800}})
    if with_scan:
        out.append({"tool": "import_file", "args": {"path": "/home/jonas/scans/model-input/drill-scan-mesh-mm.stl", "merge_faces": False, "name": "Scan reference (hidden)"}})
        sid = nid[0] + 1
        out.append({"tool": "set_part", "args": {"body": [sid, 0], "name": "Scan reference (mesh, hidden)", "color": "#7f8c99", "hidden": True}})
    return out


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "run2.json"
    json.dump(steps, open(out, "w"))
    json.dump({"parts": PARTS, "ids": nid[0]}, open(os.path.join(HERE, "parts2.json"), "w"))
    print(len(steps), "steps;", nid[0], "ids;", len(PARTS), "parts")
