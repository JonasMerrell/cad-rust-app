#!/usr/bin/env python3
"""Can each machined part be cut on the DMC2 with ONE 1/4 in flat end mill, from at most two sides?

    dfm.py <iteration>      -> iter<N>/dfm.json and a table

For each machined part (its STL, turned so that the direction it is cut from is Z):
  - it is sliced wherever its outline changes;
  - at each level everything that is not material must be reachable by the tool coming straight down from one face
    (first setup) or straight up from the other (second setup, the part flipped) without touching material in between;
  - reachable = a circle of the tool's diameter (6.35 + 0.1 mm margin) fits: no inside corner tighter than the tool,
    no hole or slot narrower than it;
  - nothing is cut deeper than REACH below the face it is cut from (24 mm: the tool's normal stick-out; cuts deeper than
    the 19.05 mm flute are flagged, they are done in steps in a pocket or bore wider than the tool);
  - the part must fit the machine's travel with the tool running round it.
The slicing and reach test are the ones the toolchanger's machined plates were checked with (~/dmc2/scripts/machining.py).
"""
import json, sys, os, struct, subprocess
import numpy as np
from shapely.geometry import LineString, Polygon, box
from shapely.ops import polygonize, unary_union
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build import ROOT

RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
TOOL = 6.35; MARGIN = 0.1; FLUTE = 19.05; REACH = 24.0; EPS = 0.02
TRAVEL = (343.0, 234.0, 150.0)
AXIS = {"Base plate": "z", "Base plate (steel)": "z", "Tailstock base": "z"}            # every other machined part is cut along the rotation axis, x

def stl(path):
    b = open(path, "rb").read(); n = struct.unpack("<I", b[80:84])[0]
    a = np.frombuffer(b[84:84 + n*50], dtype=np.dtype([("n", "<3f4"), ("v", "<9f4"), ("a", "<u2")]))
    return a["v"].reshape(-1, 3, 3).astype(float)

def section(tri, z):
    """The material's outline at height z, as a shapely (multi)polygon."""
    lo = tri[:, :, 2].min(1); hi = tri[:, :, 2].max(1)
    segs = []
    for t in tri[(lo < z) & (hi > z)]:
        pts = []
        for i in range(3):
            a, b = t[i], t[(i + 1) % 3]
            if (a[2] - z)*(b[2] - z) < 0:
                f = (z - a[2])/(b[2] - a[2]); pts.append((a[0] + f*(b[0] - a[0]), a[1] + f*(b[1] - a[1])))
        if len(pts) == 2 and pts[0] != pts[1]: segs.append(LineString([np.round(pts[0], 5), np.round(pts[1], 5)]))
    faces = list(polygonize(unary_union(segs)))
    # a face is material if a point inside it is inside the mesh: count the triangles above it
    keep = []
    x0, y0 = tri[:, :, 0], tri[:, :, 1]
    for f in faces:
        px, py = f.representative_point().coords[0]
        d1 = (x0[:, 1] - x0[:, 0])*(py - y0[:, 0]) - (y0[:, 1] - y0[:, 0])*(px - x0[:, 0])
        d2 = (x0[:, 2] - x0[:, 1])*(py - y0[:, 1]) - (y0[:, 2] - y0[:, 1])*(px - x0[:, 1])
        d3 = (x0[:, 0] - x0[:, 2])*(py - y0[:, 2]) - (y0[:, 0] - y0[:, 2])*(px - x0[:, 2])
        inside = ((d1 >= 0) & (d2 >= 0) & (d3 >= 0)) | ((d1 <= 0) & (d2 <= 0) & (d3 <= 0))
        above = 0
        for t in tri[inside]:
            nrm = np.cross(t[1] - t[0], t[2] - t[0])
            if abs(nrm[2]) < 1e-9: continue
            zz = t[0][2] - (nrm[0]*(px - t[0][0]) + nrm[1]*(py - t[0][1]))/nrm[2]
            if zz > z: above += 1
        if above % 2 == 1: keep.append(f)
    return unary_union(keep) if keep else Polygon()

def check(tri, tool_d, flute):
    zs = sorted(set(np.round(tri[:, :, 2].ravel(), 2)))
    zs = [z for i, z in enumerate(zs) if i == 0 or z - zs[i - 1] > 0.05]
    fine = [zs[0]]                                  # split thick levels, so a through bore or the outside profile can be shared between the two setups
    for z in zs[1:]:
        n = max(1, int(np.ceil((z - fine[-1])/2.0))); a0 = fine[-1]
        fine += [a0 + (z - a0)*(k + 1)/n for k in range(n)]
    zs = fine
    z0, z1 = zs[0], zs[-1]
    layers = [(a, b, section(tri, (a + b)/2)) for a, b in zip(zs[:-1], zs[1:])]
    xy = tri[:, :, :2].reshape(-1, 2)
    m = tool_d + 2
    B = box(xy[:, 0].min() - 2*m, xy[:, 1].min() - 2*m, xy[:, 0].max() + 2*m, xy[:, 1].max() + 2*m)     # room for the tool all round
    W = box(xy[:, 0].min() - m, xy[:, 1].min() - m, xy[:, 0].max() + m, xy[:, 1].max() + m)             # where the cutting is judged
    r = tool_d/2
    def reach(blocked):            # where the tool's cut reaches when `blocked` is all the material in its way
        free = B.difference(blocked)
        return free.buffer(-r, quad_segs=32).buffer(r + 1e-4, quad_segs=32)
    worst = []; need_bottom = False; deepest = 0.0
    for i, (a, b, M) in enumerate(layers):
        void = W.difference(M)
        top_block = unary_union([L[2] for L in layers[i:]])          # this layer and everything above it
        bot_block = unary_union([L[2] for L in layers[:i + 1]])
        t_ok = (z1 - a) <= flute + 1e-6; b_ok = (b - z0) <= flute + 1e-6
        from_top = reach(top_block) if t_ok else Polygon()
        left = void.difference(from_top).buffer(-EPS)
        if left.is_empty or left.area <= 0.02:
            deepest = max(deepest, z1 - a); continue                 # all of this layer is cut from the top
        from_bot = reach(bot_block) if b_ok else Polygon()
        left = void.difference(unary_union([from_top, from_bot])).buffer(-EPS)
        if left.is_empty or left.area <= 0.02:
            need_bottom = True; deepest = max(deepest, b - z0)
        else:
            c = left.representative_point()
            worst.append({"z": [float(round(a, 2)), float(round(b, 2))], "area_mm2": round(float(left.buffer(EPS).area), 2), "at": [round(float(c.x), 1), round(float(c.y), 1)]})
    return {"ok": not worst, "cannot_cut": worst, "setups": 2 if need_bottom else 1, "deepest_cut_mm": round(float(deepest), 2),
            "thickness_mm": round(float(z1 - z0), 2), "stock_xy_mm": [round(float(np.ptp(xy[:, 0])), 1), round(float(np.ptp(xy[:, 1])), 1)]}



def main(it):
    out = it if os.path.isdir(str(it)) else os.path.join(ROOT, f"iter{it}"); meta = json.load(open(os.path.join(out, "meta.json")))
    parts = [q for q in meta["parts"] if q["kind"] == "machined"]
    steps = []
    for k, q in enumerate(parts):
        steps += [{"tool": "new_document", "args": {"units": "mm"}},
                  {"tool": "import_file", "args": {"path": os.path.join(out, "parts", q["stem"] + ".step")}},
                  {"tool": "export", "args": {"format": "stl", "path": os.path.join(out, "parts", q["stem"] + ".stl")}}]
    json.dump(steps, open(os.path.join(out, "dfm-run.json"), "w"))
    subprocess.run([RCAD, "run", os.path.join(out, "dfm-run.json")], cwd=out, capture_output=True)
    res = {}; bad = []
    for q in parts:
        tri = stl(os.path.join(out, "parts", q["stem"] + ".stl"))
        ax = AXIS.get(q["name"], "x")
        if ax == "x": tri = tri[:, :, [1, 2, 0]]                 # x becomes the tool axis
        r = check(tri, TOOL + MARGIN, FLUTE)                   # first within the flute length from each side
        if not r["ok"]: r = check(tri, TOOL + MARGIN, REACH)    # else allow stepped cuts down to the tool's stick-out
        lo, hi = TOOL, 14.0
        if r["ok"]:
            for _ in range(7):
                mid = (lo + hi)/2
                if check(tri, mid, 1e9)["ok"]: lo = mid
                else: hi = mid
        r["largest_tool_mm"] = round(lo, 2) if r["ok"] else None
        r["cut_along"] = ax; r["stock"] = q["stock"]
        r["deeper_than_flute"] = r["deepest_cut_mm"] > FLUTE + 1e-6
        sx, sy = r["stock_xy_mm"]
        r["fits_travel"] = (max(sx, sy) + TOOL + 4 <= TRAVEL[0]) and (min(sx, sy) + TOOL + 4 <= TRAVEL[1])
        res[q["name"]] = r
        if not (r["ok"] and r["fits_travel"]): bad.append(q["name"])
        print(f"{q['name']:28s} {'OK  ' if r['ok'] else 'FAIL'} {sx:6.1f} x {sy:6.1f} x {r['thickness_mm']:5.2f}  setups {r['setups']}  deepest cut {r['deepest_cut_mm']:5.2f}"
              f"{' (>flute)' if r['deeper_than_flute'] else ''}  largest tool that fits {r['largest_tool_mm']}  travel {'ok' if r['fits_travel'] else 'NO'}  "
              f"{r['cannot_cut'][:3] if not r['ok'] else ''}")
    json.dump(res, open(os.path.join(out, "dfm.json"), "w"), indent=1)
    print(f"{len(res) - len(bad)} of {len(res)} machined parts pass" + (f"; FAIL: {bad}" if bad else ""))


if __name__ == "__main__":
    main(os.path.abspath(sys.argv[1]) if os.path.isdir(sys.argv[1]) else int(sys.argv[1]))
