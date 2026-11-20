#!/usr/bin/env python3
"""Pictures that need their own rCAD run: a section through the spindle and reducer, and the exploded view.

    extras.py <iteration>      -> iter<N>/render-section.png, render-section-close.png, render-exploded.png
"""
import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import R, run, failures
from build import params, stack, ROOT

axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")

# how far each station moves along x in the exploded view (mm), and down for the bases
EXPLODE = dict(vise=(150, 0), face=(110, 0), nose=(75, 0), ru=(40, 0), p1=(0, 0), brake=(-35, 0), rotor=(-70, 0), c=(-110, 0), hss=(-175, 0),
               d=(-215, 0), motor=(-270, 0), base=(0, -70), tail=(60, 0), tailbase=(60, -70), splash=(55, 0))


def section(it, out):
    p = params(it); x = stack(p)
    asm = os.path.join(out, "a-axis-assembly.step")
    s = [{"tool": "new_document", "args": {"units": "mm"}},
         {"tool": "import_file", "args": {"path": asm}},
         {"tool": "create_sketch", "args": {"plane": "front"}},
         {"tool": "sketch_add", "args": {"sketch_id": 2, "items": [R(x["motor_r"] - 30, -p["H"] - 30, 260, 90)]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 2}], "depth": "400", "op": "remove"}}},
         {"tool": "render", "args": {"view": "front", "width": 2400, "height": 1100, "path": "render-section.png", "sketches": "none"}},
         {"tool": "render", "args": {"direction": [0.25, -1, 0.25], "width": 2400, "height": 1300, "path": "render-section-iso.png", "sketches": "none"}}]
    r = run(s, out, "section")
    return failures(r)


def exploded(it, out):
    meta = json.load(open(os.path.join(out, "meta.json")))
    groups = {}
    for i, name, grp in meta["instances"]:
        groups.setdefault(grp or "p1", []).append(i)
    steps = []
    for g, ids in groups.items():
        dx, dz = EXPLODE.get(g, (0, 0))
        if dx: steps.append({"instances": ids, "direction": [1 if dx > 0 else -1, 0, 0], "distance": abs(dx)})
        if dz: steps.append({"instances": ids, "direction": [0, 0, 1 if dz > 0 else -1], "distance": abs(dz)})
    s = [{"tool": "open_document", "args": {"path": os.path.join(out, "a-axis.json")}},
         {"tool": "assembly_explode", "args": {"assembly": 0, "steps": steps}},
         {"tool": "render_assembly", "args": {"assembly": 0, "explode": 1.0, "view": "iso_front_right", "width": 2600, "height": 1500, "path": "render-exploded.png"}},
         {"tool": "render_assembly", "args": {"assembly": 0, "explode": 1.0, "view": "front", "width": 2600, "height": 1000, "path": "render-exploded-front.png"}},
         {"tool": "save_document", "args": {"path": "a-axis.json"}}]
    r = run(s, out, "exploded")
    return failures(r)


if __name__ == "__main__":
    it = int(sys.argv[1]); out = os.path.join(ROOT, f"iter{it}")
    which = sys.argv[2:] or ["section", "exploded"]
    if "section" in which: print("section", section(it, out))
    if "exploded" in which: print("exploded", exploded(it, out))
