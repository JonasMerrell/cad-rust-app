#!/usr/bin/env python3
"""Finite element runs (rCAD's own solver: 10-node tetrahedra, parts bonded where they touch) for one iteration.

    fea.py <iteration>            -> iter<N>/fea/results.json and PNGs

What the model is, and is not:
  - the custom parts exactly as built (their exported STEP), the vise as a steel block, a 40 x 40 steel test bar
    in the jaws, the gear unit and the motor as dummies of the right mass;
  - every joint is BONDED (no bolt or contact compliance) and the rolling bearing is a solid steel ring:
    calc.py adds the bearing and the bolted interfaces by hand;
  - the base plate's whole underside is fixed (the table is taken as rigid).
"""
import sys, os, json, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import C, R, run, failures
axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
from build import params, stack, ROOT

import os as _os
MESH = float(_os.environ.get("FEA_MESH", "5"))
F_ROUGH = 391.0          # N, mean sideways force at 850 W / 9000 rpm (phys.cutting)


class Model:
    def __init__(self): self.steps = [{"tool": "new_document", "args": {"units": "mm"}}]; self.fid = 0; self.bodies = {}; self.mats = []
    def imp(self, name, path, material):
        self.fid += 1; self.steps.append({"tool": "import_file", "args": {"path": path, "name": name}})
        self.bodies[name] = [self.fid, 0]; self.mats.append((name, material))
    def prism(self, name, axis, a0, a1, items, material):
        base, off = {"x": ("Right", a0), "y": ("Front", -a1), "z": ("Top", a0)}[axis]
        self.fid += 1
        self.steps.append({"tool": "add_feature", "args": {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": base}, "offset": str(round(off, 4))}}})
        pl = self.fid; self.fid += 1; sk = self.fid
        self.steps.append({"tool": "create_sketch", "args": {"plane": {"kind": "feature_plane", "feature": pl}}})
        self.steps.append({"tool": "sketch_add", "args": {"sketch_id": sk, "items": items}})
        self.fid += 1
        self.steps.append({"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": sk}], "depth": str(round(a1 - a0, 4)), "op": "new"}}})
        self.bodies[name] = [self.fid, 0]; self.mats.append((name, material))
    def finish_parts(self):
        self.steps.append({"tool": "set_parts", "args": {"parts": [{"body": self.bodies[n], "name": n, "material": m} for n, m in self.mats]}})
        self.steps.append({"tool": "mass_properties", "args": {}})


def head_model(it, bar_len, with_tail=False):
    p = params(it); x = stack(p); meta = json.load(open(os.path.join(ROOT, f"iter{it}", "meta.json")))
    parts = os.path.join(ROOT, f"iter{it}", "parts"); m = Model()
    struct = [q for q in meta["parts"] if q["kind"] == "machined" and not q["name"].startswith("Tailstock")
              and q["name"] not in ("Brake piston",)]
    for q in struct: m.imp(q["name"], os.path.join(parts, q["stem"] + ".step"), "Steel" if q["material"] == "Steel" else "Aluminium 6061")
    vise = [q for q in meta["parts"] if q["name"].startswith("Vise")][0]
    m.imp("Vise", os.path.join(parts, vise["stem"] + ".step"), "Steel")
    if it > 1:
        m.prism("Bearing ring", "x", x["ru_r"], x["ru_f"], [C(0, 0, p["ru_od"]/2), C(0, 0, p["ru_id"]/2)], "Steel")
        # gear unit: flange in D's recess + body; steel ring of about 1.46 kg
        m.prism("Gear flange", "x", x["hss_fl_r"], x["hss_fl_f"], [C(0, 0, 53.5), C(0, 0, 34)], "Steel")
        m.prism("Gear body", "x", x["hss_fl_f"], x["hss_out"] - 1.0, [C(0, 0, 42.5), C(0, 0, 31)], "Steel")
    else:
        m.prism("Gear flange", "x", x["hss_fl_r"], x["hss_fl_f"], [C(0, 0, 53.5), C(0, 0, 34)], "Steel")
        m.prism("Gear body", "x", x["hss_fl_f"], x["hss_out"], [C(0, 0, 42.5)], "Steel")   # output bonded: the bearing is added by hand
    m.prism("Motor dummy", "x", x["motor_r"] + 20, x["motor_f"], [R(-42.9, -42.9, 42.9, 42.9)], "Aluminium 6061")
    m.prism("Test bar", "x", 33.0, 44.0 + bar_len, [R(-20, -20, 20, 20)], "Steel")
    m.finish_parts()
    return m, p, x


def run_head(it, out):
    """Static: rCAD's advanced mode (crackh3d, conjugate gradients: a 5 mm mesh of 500 000 nodes fits in 8 GB). It takes ONE material
    for every part, aluminium, so the steel vise, bearing ring and test bar are modelled three times too soft: conservative, and
    calc.py takes the test bar's own bending out with aluminium's modulus.  Modal: simple mode (each part's own material) on a coarser mesh."""
    res = {}
    for tag, bar in (("chuck", 10.0), ("overhang", 100.0)):
        m, p, x = head_model(it, bar)
        H = p["H"]; xe = 44.0 + bar
        fixpt = [(-100.0 if it > 1 else x["p1_r"] - 30), 30.0, -H]
        end = [{"point": [xe, 0, 0], "normal": [1, 0, 0]}]
        sims = []
        for k, (nm, f) in enumerate((("Fz", [0, 0, -F_ROUGH]), ("Fy", [0, F_ROUGH, 0]), ("Fx", [-F_ROUGH, 0, 0]))):
            bcs = [{"type": "fixed", "faces": [{"point": fixpt, "normal": [0, 0, -1]}], "name": "table"},
                   {"type": "force", "faces": end, "f": f, "name": "cutting force"}]
            m.steps.append({"tool": "create_simulation", "args": {"name": f"{tag} {nm}", "analysis": "static", "mode": "advanced", "solver": "iterative",
                                                                   "material": "Aluminium 6061", "mesh_size": MESH, "refine": False, "bcs": bcs}})
            m.steps.append({"tool": "solve_simulation", "args": {"simulation": k, "probes": [[xe, 0, 0], [0, 0, 0], [x["p1_f"], 0, p["top"]]]}})
            sims.append(nm)
        m.steps.append({"tool": "render_simulation", "args": {"simulation": 0, "field": "displacement", "view": "front", "width": 1500, "height": 900,
                                                               "path": f"fea-{tag}-Fz.png"}})
        m.steps.append({"tool": "render_simulation", "args": {"simulation": 1, "field": "displacement", "view": "top", "width": 1500, "height": 900,
                                                               "path": f"fea-{tag}-Fy.png"}})
        r = run(m.steps, out, f"head-{tag}")
        bad = failures(r)
        if bad: print("FEA failures", tag, bad[:5])
        sols = [q.get("result", {"error": q.get("error")}) for q in r if q.get("tool") == "solve_simulation"]
        mp = [q["result"] for q in r if q.get("tool") == "mass_properties" and "result" in q]
        res[tag] = dict(bar_len=bar, x_load=xe, material="Aluminium 6061 for every part (advanced mode)",
                        static={nm: (dict(max_disp_mm=s["max_displacement_mm"], at=s["max_displacement_at"], max_vm_mpa=s["max_von_mises_mpa"],
                                          vm_at=s["max_stress_at"], mesh=s["mesh"], mass_kg=s.get("mass_kg"), warnings=s.get("warnings"),
                                          tool=s["probes"][0], faceplate=s["probes"][1], plate_top=s["probes"][2]) if "max_displacement_mm" in s else s)
                                for nm, s in zip(sims, sols[:3])})
        if mp: res.setdefault("mass", {})[tag] = {q["name"]: q["mass_g"] for q in mp[0]["parts"]}
    return res


def run_steel(it, out):
    """One check of what 'every part aluminium' costs: the overhang case again in simple mode, where the vise, bearing ring and test bar are
    steel (direct solver, so a 6 mm mesh)."""
    m, p, x = head_model(it, 100.0)
    fix = [{"faces": [{"point": [(-100.0 if it > 1 else x["p1_r"] - 30), 30.0, -p["H"]], "normal": [0, 0, -1]}]}]
    m.steps.append({"tool": "create_simulation", "args": {"name": "overhang Fz, real materials", "analysis": "static", "material": "", "mesh_size": 6, "refine": False,
                                                           "supports": fix, "loads": [{"kind": "force", "value": F_ROUGH, "direction": "-z",
                                                                                       "faces": [{"point": [144.0, 0, 0], "normal": [1, 0, 0]}]}]}})
    m.steps.append({"tool": "solve_simulation", "args": {"simulation": 0, "probes": [[144.0, 0, 0], [0, 0, 0]]}})
    r = run(m.steps, out, "head-steel")
    sols = [q.get("result", {"error": q.get("error")}) for q in r if q.get("tool") == "solve_simulation"]
    if not sols or "probes" not in sols[0]: return dict(error=str(sols[:1])[:300])
    return dict(tool=sols[0]["probes"][0], faceplate=sols[0]["probes"][1], mesh=sols[0]["mesh"], material=sols[0].get("material"))


def run_modal(it, out):
    m, p, x = head_model(it, 100.0)
    fix = [{"faces": [{"point": [(-100.0 if it > 1 else x["p1_r"] - 30), 30.0, -p["H"]], "normal": [0, 0, -1]}]}]
    ms = float(os.environ.get("FEA_MESH_MODAL", "7"))
    m.steps.append({"tool": "create_simulation", "args": {"name": "modal", "analysis": "modal", "material": "", "mesh_size": ms, "refine": False, "modes": 6, "supports": fix}})
    m.steps.append({"tool": "solve_simulation", "args": {"simulation": 0, "probes": [[144.0, 0, 0], [0, 0, 0]]}})
    for k in (1, 2, 3):
        m.steps.append({"tool": "render_simulation", "args": {"simulation": 0, "mode": k, "field": "displacement", "view": "iso", "width": 1400, "height": 900,
                                                               "path": f"fea-mode{k}.png"}})
    r = run(m.steps, out, "head-modal")
    bad = failures(r)
    if bad: print("FEA failures modal", bad[:5])
    sols = [q.get("result", {"error": q.get("error")}) for q in r if q.get("tool") == "solve_simulation"]
    if not sols or "modes" not in sols[0]: return dict(error=str(sols[:1])[:400])
    s = sols[0]
    return dict(mesh_size=ms, modes=s["modes"], mass_kg=s.get("mass_kg"), probes=s.get("probes"), mesh=s["mesh"])


def run_tail(it, out):
    """Tailstock alone: 100 N on the front face of the 12 mm centre pin where it leaves the plate's clamp (the cone itself is short and
    stiff; putting the load on the modelled needle tip only measures the tip)."""
    p = params(it); x = stack(p); meta = json.load(open(os.path.join(ROOT, f"iter{it}", "meta.json")))
    parts = os.path.join(ROOT, f"iter{it}", "parts"); m = Model()
    for q in meta["parts"]:
        if q["name"].startswith("Tailstock"):
            m.imp(q["name"], os.path.join(parts, q["stem"] + ".step"), "Aluminium 6061")
    xt = (160.0 if it >= 6 else 169.0); H = p["H"]; zt = (-H if it >= 6 else p["bot"] - 19.05); to = p["tail_out"] - 6.0
    m.prism("Centre pin", "x", xt - to, xt + 25.4, [C(0, 0, 6.01)], "Steel")
    m.finish_parts()
    fixf = [{"point": [xt + 20, 0, zt], "normal": [0, 0, -1]}]
    tip = [{"point": [xt - to, 0, 0], "normal": [-1, 0, 0]}]
    for k, (nm, f) in enumerate((("Fz", [0, 0, -100.0]), ("Fy", [0, 100.0, 0]))):
        bcs = [{"type": "fixed", "faces": fixf, "name": "table"}, {"type": "force", "faces": tip, "f": f, "name": "load at the centre"}]
        m.steps.append({"tool": "create_simulation", "args": {"name": nm, "analysis": "static", "mode": "advanced", "solver": "iterative", "material": "",
                                                               "mesh_size": 3.0, "refine": False, "bcs": bcs}})
        m.steps.append({"tool": "solve_simulation", "args": {"simulation": k, "probes": [[xt - to, 0, 0], [xt, 0, 28.0]]}})
    m.steps.append({"tool": "render_simulation", "args": {"simulation": 0, "field": "displacement", "view": "front", "width": 1200, "height": 800, "path": "fea-tail-Fz.png"}})
    r = run(m.steps, out, "tail")
    bad = failures(r)
    if bad: print("FEA failures tail", bad[:5])
    sols = [q["result"] for q in r if q.get("tool") == "solve_simulation" and "result" in q]
    return {nm: dict(force_n=100.0, max_disp_mm=s["max_displacement_mm"], at=s["max_displacement_at"], tip=s["probes"][0], plate_top=s["probes"][1])
            for nm, s in zip(("Fz", "Fy"), sols)}


def run_brake(it, out):
    """Torsion of the brake disc: rim clamped on the friction ring, 20 N·m put in at the hub face."""
    p = params(it); x = stack(p); m = Model()
    # rotor drawn natively so that the clamped ring (r 47..61) is its own face: a 0.02 step at r = 47
    m.prism("hub", "x", x["fl_r"], x["ru_r"], [C(0, 0, 38), C(0, 0, 5.5)], "Aluminium 6061")
    m.prism("flange", "x", x["fl_r"], x["fl_r"] + min(p["web_t"], p["fl_t"] - 0.02), [C(0, 0, p["fr_ri"]), C(0, 0, 38)], "Aluminium 6061")
    m.prism("rim", "x", x["fl_r"], x["fl_f"], [C(0, 0, p["fl_r"]), C(0, 0, p["fr_ri"])], "Aluminium 6061")
    m.finish_parts()
    T = 20000.0   # N·mm
    bcs = [{"type": "fixed", "faces": [{"point": [x["fl_f"], 0, (p["fr_ri"] + p["fl_r"])/2], "normal": [1, 0, 0]}], "name": "friction ring"},
           {"type": "remote_rigid", "faces": [{"point": [x["ru_r"], 0, 30], "normal": [1, 0, 0]}], "point": [x["ru_r"], 0, 0],
            "force": [0, 0, 0], "moment": [T, 0, 0], "u": [None, None, None], "rot": [None, None, None], "name": "torque"}]
    m.steps.append({"tool": "create_simulation", "args": {"name": "brake torsion", "analysis": "static", "mode": "advanced", "material": "", "mesh_size": float(os.environ.get("FEA_MESH_BRAKE", "2.5")),
                                                           "refine": False, "bcs": bcs}})
    m.steps.append({"tool": "solve_simulation", "args": {"simulation": 0, "probes": [[x["ru_r"], 0, 30.0], [x["ru_r"], 30.0, 0], [x["fl_r"], 0, 46.0]]}})
    m.steps.append({"tool": "render_simulation", "args": {"simulation": 0, "field": "displacement", "view": "right", "width": 1000, "height": 900, "path": "fea-brake-torsion.png"}})
    r = run(m.steps, out, "brake")
    bad = failures(r)
    if bad: print("FEA failures brake", bad[:5])
    sols = [q["result"] for q in r if q.get("tool") == "solve_simulation" and "result" in q]
    if not sols: return {}
    s = sols[0]
    pr = s.get("probes", [])
    rot = None
    if pr:
        # hub face point at z = 30 moves in -y, the one at y = 30 moves in +z, for a positive moment about x
        rot = (abs(pr[0]["displacement_mm"][1])/30.0 + abs(pr[1]["displacement_mm"][2])/30.0)/2
    return dict(torque_nmm=T, rotation_rad=rot, probes=pr, max_disp_mm=s["max_displacement_mm"], at=s["max_displacement_at"], max_vm_mpa=s["max_von_mises_mpa"])


if __name__ == "__main__":
    it = int(sys.argv[1]); out = os.path.join(ROOT, f"iter{it}", "fea"); os.makedirs(out, exist_ok=True)
    which = sys.argv[2:] or ["head", "tail", "brake", "modal"]
    rp = os.path.join(out, "results.json")
    res = json.load(open(rp)) if os.path.exists(rp) else {}
    if "head" in which: res["head"] = run_head(it, out)
    if "tail" in which and it >= 2: res["tail"] = run_tail(it, out)
    if "brake" in which and 3 <= it < 6: res["brake"] = run_brake(it, out)
    if "modal" in which: res["modal"] = run_modal(it, out)
    if "steel" in which: res["steel"] = run_steel(it, out)
    json.dump(res, open(rp, "w"), indent=1)
    print(json.dumps(res, indent=1)[:3000])
