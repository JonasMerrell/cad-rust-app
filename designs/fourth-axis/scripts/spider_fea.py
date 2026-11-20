#!/usr/bin/env python3
"""FE check of carrier 2's flexure blades: the spider alone, the three stiff arms held at their sleeve seats' screw holes,
a tangential force on one B pad. Gives the blade's stiffness and stress for the preload.

    spider_fea.py <n>     -> cycloidal/c<n>/fea/spider.json
"""
import sys, os, json, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import run, failures
from build6 import ROOT
axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")

if __name__ == "__main__":
    folder = os.path.join(ROOT, "cycloidal", f"c{sys.argv[1]}"); out = os.path.join(folder, "fea"); os.makedirs(out, exist_ok=True)
    meta = json.load(open(os.path.join(folder, "meta.json"))); p, x = meta["params"], meta["x"]
    q = [k for k in meta["parts"] if k["name"].startswith("Carrier 2")][0]
    RO = p["RO"]; F = 100.0; xm = (x["car2_f"] + x["car2_r"])/2
    pt = lambda r, a: (r*math.cos(math.radians(a)), r*math.sin(math.radians(a)))
    by, bz = pt(RO, 60); ty, tz = -math.sin(math.radians(60)), math.cos(math.radians(60))
    fixed = [{"point": [xm, *pt(RO + 3.3, a)], "normal": [0, *pt(-1.0, a)]} for a in (0, 120, 240)]            # the A screw holes' walls
    s = [{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "import_file", "args": {"path": os.path.join(folder, "parts", q["stem"] + ".step")}},
         {"tool": "create_simulation", "args": {"name": "blade", "analysis": "static", "mode": "advanced", "solver": "iterative", "material": "Steel", "mesh_size": 1.2, "refine": False,
                                                "bcs": [{"type": "fixed", "faces": fixed, "name": "A seats"},
                                                        {"type": "force", "faces": [{"point": [x["slB_r"], by + 4.2*ty, bz + 4.2*tz], "normal": [1, 0, 0]}], "f": [0, F*ty, F*tz], "name": "sleeve B"}]}},
         {"tool": "solve_simulation", "args": {"simulation": 0, "probes": [[x["slB_r"], by + 4.2*ty, bz + 4.2*tz], [x["car2_f"], by, bz + 6.0]]}},
         {"tool": "render_simulation", "args": {"simulation": 0, "field": "displacement", "view": "right", "width": 1400, "height": 1100, "path": "fea-spider.png"}},
         {"tool": "render_simulation", "args": {"simulation": 0, "field": "stress", "view": "right", "width": 1400, "height": 1100, "path": "fea-spider-stress.png"}}]
    r = run(s, out, "spider")
    print(failures(r))
    sol = [k.get("result", {"error": k.get("error")}) for k in r if k.get("tool") == "solve_simulation"]
    if sol and "probes" in sol[0]:
        d = sol[0]["probes"][0]["displacement_mm"]; ut = d[1]*ty + d[2]*tz
        res = dict(force_n=F, tangential_mm=ut, k_n_per_mm=F/ut, max_vm_mpa=sol[0]["max_von_mises_mpa"], vm_per_n=sol[0]["max_von_mises_mpa"]/F, mesh=sol[0]["mesh"], vm_at=sol[0]["max_stress_at"])
        json.dump(res, open(os.path.join(out, "spider.json"), "w"), indent=1); print(res)
    else: print(str(sol)[:600])
