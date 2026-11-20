#!/usr/bin/env python3
"""Run rCAD's own stock simulation on plain side-milling cuts with the 1/4 in 3-flute tool to read the force it
predicts at a given spindle power: the load cases of the A axis come from here (and from phys.cutting, the same formulas)."""
import json, subprocess, os, sys
RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
M = {"name": "DMC2", "travel": [343, 234, 150], "max_feed": 3000, "rapid_z": 3000, "tool_changer": False, "spin_up": 4,
     "spindle_power": 2600, "spindle_efficiency": 0.43, "spindle_rated_rpm": 9000, "spindle_above_rated": 0.76,
     "spindle_never_exceed": 0.85, "spindle_caution": 0.76}
rows = []
for rpm, feed, ap, ae in [(9000, 3000, 6, 1.0), (9000, 3000, 6, 1.6), (9000, 3000, 6, 2.0), (9000, 3000, 6, 2.3), (9000, 3000, 12, 1.0),
                          (24000, 3000, 6, 0.6), (24000, 3000, 6, 1.0), (24000, 3000, 12, 0.3)]:
    s = [{"tool": "new_document", "args": {"units": "mm"}},
         {"tool": "create_sketch", "args": {"plane": "top"}},
         {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [{"type": "rect", "corner1": [0, 0], "corner2": [80, 40]}]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": str(ap)}}},
         {"tool": "create_cam", "args": {"name": "cut", "stock": {"side": ae, "top": 0, "bottom": 0, "material": "Aluminium"}, "machine": M, "max_rpm": 24000}},
         {"tool": "cam_tool", "args": {"cam": 0, "kind": "flat", "name": "T24", "diameter": 6.35, "flutes": 3, "flute_length": 19.05, "stickout": 24,
                                       "material": "carbide", "rpm": rpm, "feed": feed, "stepdown": ap}},
         {"tool": "cam_op", "args": {"cam": 0, "kind": "contour", "tool": 1, "side": "outside", "stepdown": ap, "max_engagement": 0.6, "rpm": rpm, "feed": feed,
                                     "finish_lap": 0, "lead": 3}},
         {"tool": "simulate_cam", "args": {"cam": 0}}]
    json.dump(s, open("c.json", "w"))
    out = subprocess.run([RCAD, "run", "c.json"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        j = json.loads(line)
        if "error" in j: print("ERR", j["tool"], j["error"][:300])
        if j.get("tool") == "simulate_cam" and "result" in j:
            for l in j["result"].get("loads", []):
                rows.append(dict(rpm=rpm, feed=feed, ap=ap, ae=ae, power_w=l["power_w"], force_n=l["force_n"], torque_nm=l["torque_nm"],
                                 deflection_mm=l["deflection_mm"], mrr=l["mrr_cm3_min"], rating=l["power_rating"], width=l.get("width")))
json.dump(rows, open("cutsim.json", "w"), indent=1)
for r in rows: print(r)
