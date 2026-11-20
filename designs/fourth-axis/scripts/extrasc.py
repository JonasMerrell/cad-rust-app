#!/usr/bin/env python3
"""Pictures of a cycloidal iteration that need their own rCAD run: a lengthwise section, a cross-section through disc A
(the reducer seen from the front) and the exploded view.

    extrasc.py <n> [section] [cross] [exploded]
"""
import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import R, run, failures
from build6 import ROOT
axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")

EXPLODE = dict(vise=(190, 0), face=(150, 0), nose=(115, 0), splash=(95, 0), ru=(80, 0), p1=(40, 0), brake=(10, 0), rotor=(-15, 0), huba=(55, 0), c=(-45, 0), hubb=(-80, 0),
               car1=(-110, 0), g1=(-150, 0), discA=(-185, 0), m=(-215, 0), discB=(-245, 0), pins=(-150, 60), sleeves=(-280, 0), g2=(-310, 0), car2=(-345, 0), shaft=(-420, 0),
               d=(-390, 0), motor=(-390, 0), belt=(-440, 0), guard=(-480, 0), base=(0, -90), tail=(60, 0), tailbase=(60, -90))


def section(out, meta):
    p, x = meta["params"], meta["x"]; asm = os.path.join(out, "a-axis-assembly.step")
    s = [{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "import_file", "args": {"path": asm}},
         {"tool": "create_sketch", "args": {"plane": "front"}},
         {"tool": "sketch_add", "args": {"sketch_id": 2, "items": [R(x["motor_r"] - 30, -p["H"] - 30, 260, 90)]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 2}], "depth": "400", "op": "remove"}}},
         {"tool": "render", "args": {"view": "front", "width": 3200, "height": 1500, "path": "render-section.png", "sketches": "none"}},
         {"tool": "render", "args": {"direction": [0.25, -1, 0.25], "width": 2400, "height": 1300, "path": "render-section-iso.png", "sketches": "none"}}]
    r = run(s, out, "section"); return failures(r)


def cross(out, meta):
    p, x = meta["params"], meta["x"]; asm = os.path.join(out, "a-axis-assembly.step")
    xc = (x["dA_f"] + x["dA_r"])/2
    s = [{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "import_file", "args": {"path": asm}},
         {"tool": "add_feature", "args": {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": "Right"}, "offset": str(round(xc, 3))}}},
         {"tool": "create_sketch", "args": {"plane": {"kind": "feature_plane", "feature": 2}}},
         {"tool": "sketch_add", "args": {"sketch_id": 3, "items": [R(-200, -120, 200, 120)]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 3}], "depth": "400", "op": "remove"}}},
         {"tool": "render", "args": {"view": "right", "width": 2600, "height": 1700, "path": "render-cross-disc.png", "sketches": "none"}}]
    r = run(s, out, "cross"); return failures(r)


def exploded(out, meta):
    groups = {}
    for i, name, grp in meta["instances"]: groups.setdefault(grp or "p1", []).append(i)
    steps = []
    for g, ids in groups.items():
        dx, dz = EXPLODE.get(g, (0, 0))
        if dx: steps.append({"instances": ids, "direction": [1 if dx > 0 else -1, 0, 0], "distance": abs(dx)})
        if dz: steps.append({"instances": ids, "direction": [0, 0, 1 if dz > 0 else -1], "distance": abs(dz)})
    s = [{"tool": "open_document", "args": {"path": os.path.join(out, "a-axis.json")}},
         {"tool": "assembly_explode", "args": {"assembly": 0, "steps": steps}},
         {"tool": "render_assembly", "args": {"assembly": 0, "explode": 1.0, "view": "iso_front_right", "width": 3000, "height": 1700, "path": "render-exploded.png"}},
         {"tool": "render_assembly", "args": {"assembly": 0, "explode": 1.0, "view": "iso_back_right", "width": 3000, "height": 1700, "path": "render-exploded-back.png"}},
         {"tool": "render_assembly", "args": {"assembly": 0, "explode": 1.0, "view": "front", "width": 3200, "height": 1000, "path": "render-exploded-front.png"}},
         {"tool": "save_document", "args": {"path": "a-axis.json"}}]
    r = run(s, out, "exploded"); return failures(r)


if __name__ == "__main__":
    out = os.path.join(ROOT, "cycloidal", f"c{sys.argv[1]}"); meta = json.load(open(os.path.join(out, "meta.json")))
    which = sys.argv[2:] or ["section", "cross", "exploded"]
    for w in which: print(w, {"section": section, "cross": cross, "exploded": exploded}[w](out, meta))
