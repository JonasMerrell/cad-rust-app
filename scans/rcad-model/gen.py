"""DeWalt DC742 drill as a native rCAD Part Studio, fitted to the drill scan (mm, scan frame:
standing on XY, Z up, chuck towards -X). Writes run.json for `rcad run`.

The head (chuck, clutch, housing, rear cap) is built on a local axis along +X through the origin,
then rotated about Y by TILT and moved to the measured axis (Y0, Z0 at x=0).
"""
import json, math, sys

TILT = -5.71          # deg: the head axis rises 0.1 mm per mm towards the rear (fitted to the scan)
Y0, Z0 = 22.5, 203.9  # head axis at x = 0 (fitted)
YC = 23.7             # grip / trigger mid-plane

YEL, BLK, SIL, DRK = "#f2b705", "#1c1c1c", "#b9b9b9", "#3a3a3a"

steps = [{"tool": "new_document", "args": {}}]
nid = [0]
expect = {}


def new_id():
    nid[0] += 1
    return nid[0]


def plane(base, offset):
    i = new_id()
    steps.append({"tool": "add_feature", "args": {"kind": "plane", "name": f"Plane {base} {offset}",
                  "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": base}, "offset": str(offset)}}})
    return i


def sketch(pl, items, name=None):
    i = new_id()
    p = pl if isinstance(pl, str) else {"kind": "feature_plane", "feature": pl}
    a = {"plane": p}
    if name:
        a["name"] = name
    steps.append({"tool": "create_sketch", "args": a})
    steps.append({"tool": "sketch_add", "args": {"sketch_id": i, "items": items}})
    return i


def feat(kind, params, name):
    i = new_id()
    steps.append({"tool": "add_feature", "args": {"kind": kind, "params": params, "name": name}})
    return i


def front_plane_y(y):
    """Offset plane parallel to Front (XZ) at world y (Front's normal is -Y)."""
    return plane("Front", -y)


def top_plane_z(z):
    return plane("Top", z)


def revolve(profile, name, op="new"):
    """profile: [(x, r)] closed on the axis; drawn on Front (local head frame, axis = X)."""
    pts = [[x, r] for x, r in profile]
    s = sketch("front", [{"type": "polyline", "closed": True, "points": pts},
                         {"type": "line", "from": [-80, 0], "to": [170, 0], "construction": True}], name + " profile")
    # the construction line is the last curve: polyline has len(pts) lines -> ids len(pts)+1 .. ; line after
    axis_curve = AXIS_CURVE[0](len(pts))
    return feat("revolve", {"profiles": [{"sketch": s}], "axis": {"kind": "sketch_curve", "sketch": s, "curve": axis_curve}, "op": op}, name)


# curve ids in a fresh sketch: polyline of n points -> points 1..n, lines n+1..2n; construction line: points 2n+1, 2n+2 -> curve 2n+3
AXIS_CURVE = [lambda n: 2 * n + 3]

bodies = {}

# ---------------- head, local frame ----------------
bodies["Chuck nose"] = revolve([(-61, 0), (-61, 5), (-58, 7.5), (-53, 11), (-53, 0)], "Chuck nose")
bodies["Chuck front sleeve"] = revolve([(-53, 0), (-53, 15), (-50, 18.2), (-47, 19.8), (-26, 19.8), (-24.5, 18.5), (-24, 0)], "Chuck front sleeve")
bodies["Chuck rear sleeve"] = revolve([(-24, 0), (-24, 19.3), (-22, 20.5), (-9, 20.5), (-7, 19.3), (-7, 0)], "Chuck rear sleeve")
bodies["Chuck ring"] = revolve([(-7, 0), (-7, 17.8), (-2, 17.8), (-2, 0)], "Chuck ring")
bodies["Clutch collar"] = revolve([(-2, 0), (-2, 21), (-1, 21.9), (5, 25.3), (6, 25.0), (10, 27.5), (10, 0)], "Clutch collar")
bodies["Housing"] = revolve([(10, 0), (10, 27.5), (14, 30.0), (40, 31.3), (70, 31.8), (100, 31.2), (112, 30.4), (112, 0)], "Housing")
bodies["Rear cap"] = revolve([(112, 0), (112, 30.4), (122, 28.8), (128, 26.5), (133, 23), (138, 18.5), (143, 13), (147, 7), (149.5, 0)], "Rear cap")

# chuck grip ribs: groove tool bodies patterned round the axis, then cut
def groove_ring(x0, x1, r, depth, width, count, target, name):
    pl = plane("Top", r - depth)
    s = sketch(pl, [{"type": "rect", "corner1": [x0, -width / 2], "corner2": [x1, width / 2]}], name + " sketch")
    tool = feat("extrude", {"profiles": [{"sketch": s}], "depth": depth + 3, "op": "new"}, name + " tool")
    pat = feat("circularPattern", {"kind": "part", "bodies": [{"kind": "body", "body": [tool, 0]}], "axis": {"kind": "axis", "axis": 0}, "count": str(count), "angle": "360"}, name + " pattern")
    tools = [{"kind": "body", "body": [tool, 0]}] + [{"kind": "body", "body": [pat, i]} for i in range(count - 1)]
    feat("boolean", {"kind": "subtract", "target": {"kind": "body", "body": [target, 0]}, "tools": tools}, name)

GROOVES = [(-47, -27, 19.8, 1.2, 2.2, 16, bodies["Chuck front sleeve"], "Chuck front sleeve grip ribs"),
           (-21, -10, 20.5, 1.0, 2.0, 24, bodies["Chuck rear sleeve"], "Chuck rear sleeve grip ribs")]
for g in GROOVES:
    groove_ring(*g)

# speed selector on top: raised yellow frame, pocket, black slider
s = sketch(plane("Top", 26), [{"type": "rect", "corner1": [24, -11.5], "corner2": [68, 11.5]}], "Speed selector frame sketch")
_fr = feat("extrude", {"profiles": [{"sketch": s}], "depth": 9, "op": "new"}, "Speed selector frame")
feat("boolean", {"kind": "union", "target": {"kind": "body", "body": [bodies["Housing"], 0]}, "tools": [{"kind": "body", "body": [_fr, 0]}]}, "Speed selector frame onto housing")
s = sketch(plane("Top", 35), [{"type": "rect", "corner1": [30, -7], "corner2": [62, 7]}], "Speed selector pocket sketch")
feat("extrude", {"profiles": [{"sketch": s}], "depth": 10, "end": "symmetric", "op": "remove"}, "Speed selector pocket")
s = sketch(plane("Top", 29), [{"type": "rect", "corner1": [42, -5.5], "corner2": [50, 5.5]}], "Speed selector slider sketch")
bodies["Speed selector"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 6.5, "op": "new"}, "Speed selector slider")

# black screw caps on the top shoulders
s = sketch(plane("Top", 22), [{"type": "circle", "center": [17, 15], "radius": 2.6}, {"type": "circle", "center": [17, -15], "radius": 2.6},
                               {"type": "circle", "center": [104, 15.5], "radius": 2.6}, {"type": "circle", "center": [104, -15.5], "radius": 2.6}], "Screw caps sketch")
bodies["Screw caps"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 6.0, "op": "new"}, "Screw caps")

# side vent grilles on the rear cap (both sides): 5 horizontal slots
for side in (1, -1):
    items = [{"type": "rect", "corner1": [113, z], "corner2": [127, z + 2.2]} for z in (-10.5, -6.3, -2.1, 2.1, 6.3)]
    s = sketch(plane("Front", -side * 31.5), items, f"Side vents {'+Y' if side > 0 else '-Y'} sketch")
    feat("extrude", {"profiles": [{"sketch": s}], "depth": 9, "end": "symmetric", "op": "remove"}, f"Side vents {'+Y' if side > 0 else '-Y'}")

# rear-top vent slots ("E" vent) on the rear cap
s = sketch(plane("Top", 27), [{"type": "rect", "corner1": [117, y], "corner2": [126, y + 2.2]} for y in (-9.3, -5.1, -0.9, 3.3, 7.5)], "Rear top vents sketch")
feat("extrude", {"profiles": [{"sketch": s}], "depth": 8, "end": "symmetric", "op": "remove"}, "Rear top vents")

# flat label panels: DEWALT logo (-Y) and rating label (+Y)
s = sketch(plane("Front", 33.5), [{"type": "rect", "corner1": [32, -12], "corner2": [88, 6]}], "Logo panel sketch")
feat("extrude", {"profiles": [{"sketch": s}], "depth": 10, "end": "symmetric", "op": "remove"}, "Logo panel recess (-Y)")
s = sketch(plane("Front", -33.5), [{"type": "rect", "corner1": [22, -11], "corner2": [92, 7]}], "Label panel sketch")
feat("extrude", {"profiles": [{"sketch": s}], "depth": 10, "end": "symmetric", "op": "remove"}, "Rating label recess (+Y)")

s = sketch(plane("Front", 28.75), [{"type": "rect", "corner1": [34, -10.5], "corner2": [86, 4.5]}], "Logo plate sketch")
bodies["Logo plate"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 0.7, "end": "symmetric", "op": "new"}, "DEWALT logo plate (-Y)")
s = sketch(plane("Front", -28.75), [{"type": "rect", "corner1": [24, -9.5], "corner2": [90, 5.5]}], "Rating plate sketch")
bodies["Rating plate"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 0.7, "end": "symmetric", "op": "new"}, "DC742 rating label plate (+Y)")

HEAD = ["Logo plate", "Rating plate", "Chuck nose", "Chuck front sleeve", "Chuck rear sleeve", "Chuck ring", "Clutch collar", "Housing", "Rear cap", "Speed selector", "Screw caps"]

head_refs = [{"kind": "body", "body": [bodies[n], 0]} for n in HEAD] + [{"kind": "body", "body": [bodies["Screw caps"], i]} for i in (1, 2, 3)]
feat("transform", {"mode": "rotate", "bodies": head_refs, "axis": {"kind": "axis", "axis": 1}, "angle": str(TILT)}, "Tilt head to the scanned axis")
feat("transform", {"mode": "translate", "bodies": head_refs, "direction": {"kind": "axis", "axis": 1}, "distance": str(Y0)}, "Head to Y")
feat("transform", {"mode": "translate", "bodies": head_refs, "direction": {"kind": "axis", "axis": 2}, "distance": str(Z0)}, "Head to Z")

# ---------------- grip (world frame) ----------------
secs = [(66, 98.0, YC, 26.0, 21.5), (100, 93.5, YC, 25.5, 20.6), (128, 86.5, YC, 24.2, 20.3), (160, 81.0, YC, 21.5, 18.0), (188, 80.0, YC, 23.0, 19.0)]
profs = []
for z, cx, cy, a, b in secs:
    s = sketch(plane("Top", z), [{"type": "ellipse", "center": [cx, cy], "major_end": [cx + a, cy], "minor_radius": b}], f"Grip section z{z}")
    profs.append({"sketch": s})
bodies["Grip"] = feat("loft", {"profiles": profs, "op": "new"}, "Grip (rubber overmould)")

# yellow front spine of the grip
pl_mid = plane("Front", -YC)
spine = [[71.5, 64], [64, 100], [61, 128], [62, 132], [70, 132], [72, 100], [79, 64]]
s = sketch(pl_mid, [{"type": "polyline", "closed": True, "points": spine}], "Grip front spine sketch")
bodies["Grip spine"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 24, "end": "symmetric", "op": "new"}, "Grip front spine (yellow)")

# trigger
trig = [[66, 128], [55, 130], [47.5, 134], [42.5, 141], [40.5, 150], [41, 160], [43.5, 167], [48, 171.5], [66, 173]]
s = sketch(pl_mid, [{"type": "polyline", "closed": True, "points": trig}], "Trigger sketch")
bodies["Trigger"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 15, "end": "symmetric", "op": "new"}, "Trigger")

# forward/reverse button through the top of the grip
s = sketch(pl_mid, [{"type": "circle", "center": [52, 177], "radius": 4.2}], "F/R button sketch")
bodies["F/R button"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 50, "end": "symmetric", "op": "new"}, "Forward/reverse button")

# ---------------- battery (world frame) ----------------
def rrect(cx, cy, w, h, r):
    x0, x1, y0, y1 = cx - w / 2, cx + w / 2, cy - h / 2, cy + h / 2
    k = r * (1 - 1 / math.sqrt(2))
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

BX, BY, BW, BD = 89.8, 25.3, 93.6, 84.2
s0 = sketch("top", rrect(BX, BY, BW - 8, BD - 9, 9), "Battery foot sketch")
s1 = sketch(plane("Top", 24), rrect(BX, BY, BW, BD, 11), "Battery base waist sketch")
bodies["Battery base"] = feat("loft", {"profiles": [{"sketch": s0}, {"sketch": s1}], "op": "new"}, "Battery base foot (black)")
_bb = feat("extrude", {"profiles": [{"sketch": s1}], "depth": 16, "op": "new"}, "Battery base upper (black)")
feat("boolean", {"kind": "union", "target": {"kind": "body", "body": [bodies["Battery base"], 0]}, "tools": [{"kind": "body", "body": [_bb, 0]}]}, "Battery base")
s = sketch(plane("Top", 40), rrect(BX, BY, BW, BD, 11), "Battery collar sketch")
bodies["Battery collar"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 22, "op": "new"}, "Battery collar (yellow)")
s1 = sketch(plane("Top", 62), rrect(93.0, 25.0, 78, 64, 9), "Battery stem base")
s2 = sketch(plane("Top", 78), rrect(97.5, 24.0, 58, 44, 8), "Battery stem top")
bodies["Battery stem"] = feat("loft", {"profiles": [{"sketch": s1}, {"sketch": s2}], "op": "new"}, "Battery stem (yellow)")

# label recess and ribs on the black base, release buttons on both sides
s = sketch(plane("Front", 16.8 + 0.0), [{"type": "rect", "corner1": [66, 7], "corner2": [114, 31]}], "Battery label sketch")
feat("extrude", {"profiles": [{"sketch": s}], "depth": 2, "end": "symmetric", "op": "remove"}, "Battery label recess (-Y)")
for side in (1, -1):
    yface = BY + side * (BD / 2 - 1.2)
    s = sketch(plane("Front", -yface), [{"type": "rect", "corner1": [79, 36], "corner2": [101, 56]}], f"Release button {'+Y' if side > 0 else '-Y'} sketch")
    bodies[f"Release button {'+Y' if side > 0 else '-Y'}"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 8, "end": "symmetric", "op": "new"}, f"Battery release button {'+Y' if side > 0 else '-Y'}")
    yb = BY + side * (BD / 2)
    s = sketch(plane("Front", -yb), [{"type": "rect", "corner1": [82, z], "corner2": [98, z + 1.2]} for z in (27.5, 30.5, 33.5)], f"Latch ridges {'+Y' if side > 0 else '-Y'} sketch")
    bodies[f"Latch ridges {'+Y' if side > 0 else '-Y'}"] = feat("extrude", {"profiles": [{"sketch": s}], "depth": 3, "end": "symmetric", "op": "new"}, f"Battery latch ridges {'+Y' if side > 0 else '-Y'}")

COLORS = {"Logo plate": BLK, "Rating plate": BLK, "Chuck nose": DRK, "Chuck front sleeve": BLK, "Chuck rear sleeve": BLK, "Chuck ring": SIL, "Clutch collar": BLK,
          "Housing": YEL, "Rear cap": BLK, "Speed selector": BLK, "Screw caps": BLK, "Grip": BLK, "Grip spine": YEL,
          "Trigger": BLK, "F/R button": BLK, "Battery base": BLK, "Battery collar": YEL, "Battery stem": YEL,
          "Release button +Y": YEL, "Release button -Y": YEL, "Latch ridges +Y": BLK, "Latch ridges -Y": BLK}

if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "run.json"
    json.dump(steps, open(out, "w"), indent=0)
    json.dump({"bodies": bodies, "colors": COLORS, "grooves": GROOVES}, open("bodies.json", "w"))
    print(len(steps), "steps;", nid[0], "ids")


def finish(prefix, with_scan=False):
    """Name and colour the parts, export, render; optionally add the scan as a hidden reference."""
    names = dict(bodies)
    multi = {"Screw caps": 4, "Latch ridges +Y": 3, "Latch ridges -Y": 3}
    out = []
    for n, f in names.items():
        for i in range(multi.get(n, 1)):
            nm = n if multi.get(n, 1) == 1 else f"{n} {i + 1}"
            mat = "ABS"
            out.append({"tool": "set_part", "args": {"body": [f, i], "name": nm, "color": COLORS[n], "finish": "satin" if COLORS[n] != SIL else "metal"}})
    out.append({"tool": "export", "args": {"path": f"{prefix}.stl", "format": "stl"}})
    out.append({"tool": "export", "args": {"path": f"{prefix}.step", "format": "step"}})
    for v in ("iso_front_left", "iso", "left", "front", "top", "back"):
        out.append({"tool": "render", "args": {"path": f"{prefix}-{v}.png", "view": v, "width": 1200, "height": 900}})
    if with_scan:
        out.append({"tool": "import_file", "args": {"path": "/home/jonas/scans/model-input/drill-scan-mesh-mm.stl", "merge_faces": False, "name": "Scan reference (hidden)"}})
    return out
