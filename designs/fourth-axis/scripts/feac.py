#!/usr/bin/env python3
"""Finite element runs for a cycloidal iteration (cycloidal/c<n>): the same model rules as fea.py.

    feac.py <n> [head] [modal]

The structure: every machined housing and spindle part as built, the vise, a 40 x 40 test bar, the main bearing as a solid
ring, the motor as a block of its mass on the ear. The reducer's moving parts (discs, bushes, sleeves, carrier 2, shaft) are
left out: they carry torque, not the cutting force's bending, and torsion is done by hand in calcc.py.
"""
import sys, os, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import C, R, rrect, run, failures
axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
from fea import Model, MESH, F_ROUGH
from build6 import ROOT

SKIP = ("Tailstock", "Brake piston", "Cycloid disc", "Eccentric bush", "Carrier 2")


def head_model(folder, bar_len, modal=False):
    meta = json.load(open(os.path.join(folder, "meta.json"))); p = meta["params"]; x = meta["x"]
    parts = os.path.join(folder, "parts"); m = Model()
    # modal runs use a 7 mm mesh, which drops the 3 mm brake disc and would leave the hub spacer and carrier 1 floating:
    # those three become one steel cylinder of the same length bonded to hub A
    skip = SKIP + (("Brake disc", "Hub spacer", "Carrier 1") if modal else ())
    if modal: m.prism("Rotor dummy", "x", x["car1_r"], x["hubA_r"], [C(0, 0, 30.0)], "Steel")
    for q in meta["parts"]:
        if q["kind"] != "machined" or q["name"].startswith(skip): continue
        m.imp(q["name"], os.path.join(parts, q["stem"] + ".step"), "Steel" if q["material"] == "Steel" else "Aluminium 6061")
    vise = [q for q in meta["parts"] if q["name"].startswith("Vise")][0]
    m.imp("Vise", os.path.join(parts, vise["stem"] + ".step"), "Steel")
    m.prism("Bearing ring", "x", x["ru_r"], x["ru_f"], [C(0, 0, p["ru_od"]/2), C(0, 0, p["ru_id"]/2)], "Steel")
    ym, zm = p["ym"], p["zm"]
    m.prism("Motor dummy", "x", x["web_f"], x["web_f"] + 85.0, rrect(ym - 28, zm - 28, ym + 28, zm + 28, 4), "Aluminium 6061")   # 0.72 kg as aluminium; the motor weighs 1.2
    m.prism("Test bar", "x", 33.0, 44.0 + bar_len, [R(-20, -20, 20, 20)], "Steel")
    m.finish_parts()
    return m, p, x


def run_head(folder, out):
    res = {}
    for tag, bar in (("chuck", 10.0), ("overhang", 100.0)):
        m, p, x = head_model(folder, bar)
        H = p["H"]; xe = 44.0 + bar
        fixpt = [-100.0, 30.0, -H]; end = [{"point": [xe, 0, 0], "normal": [1, 0, 0]}]
        sims = []
        for k, (nm, f) in enumerate((("Fz", [0, 0, -F_ROUGH]), ("Fy", [0, F_ROUGH, 0]), ("Fx", [-F_ROUGH, 0, 0]))):
            bcs = [{"type": "fixed", "faces": [{"point": fixpt, "normal": [0, 0, -1]}], "name": "table"},
                   {"type": "force", "faces": end, "f": f, "name": "cutting force"}]
            m.steps.append({"tool": "create_simulation", "args": {"name": f"{tag} {nm}", "analysis": "static", "mode": "advanced", "solver": "iterative",
                                                                   "material": "Aluminium 6061", "mesh_size": MESH, "refine": False, "bcs": bcs}})
            m.steps.append({"tool": "solve_simulation", "args": {"simulation": k, "probes": [[xe, 0, 0], [0, 0, 0], [x["p1_f"], 0, p["top"]]]}})
            sims.append(nm)
        m.steps.append({"tool": "render_simulation", "args": {"simulation": 0, "field": "displacement", "view": "front", "width": 1500, "height": 900, "path": f"fea-{tag}-Fz.png"}})
        m.steps.append({"tool": "render_simulation", "args": {"simulation": 1, "field": "displacement", "view": "top", "width": 1500, "height": 900, "path": f"fea-{tag}-Fy.png"}})
        r = run(m.steps, out, f"head-{tag}")
        bad = failures(r)
        if bad: print("FEA failures", tag, bad[:5])
        sols = [q.get("result", {"error": q.get("error")}) for q in r if q.get("tool") == "solve_simulation"]
        res[tag] = dict(bar_len=bar, x_load=xe, material="Aluminium 6061 for every part (advanced mode)",
                        static={nm: (dict(max_disp_mm=s["max_displacement_mm"], at=s["max_displacement_at"], max_vm_mpa=s["max_von_mises_mpa"], vm_at=s["max_stress_at"],
                                          mesh=s["mesh"], mass_kg=s.get("mass_kg"), warnings=s.get("warnings"),
                                          tool=s["probes"][0], faceplate=s["probes"][1], plate_top=s["probes"][2]) if "max_displacement_mm" in s else s)
                                for nm, s in zip(sims, sols[:3])})
    return res


def run_modal(folder, out):
    m, p, x = head_model(folder, 100.0, modal=True)
    fix = [{"faces": [{"point": [-100.0, 30.0, -p["H"]], "normal": [0, 0, -1]}]}]
    ms = float(os.environ.get("FEA_MESH_MODAL", "7"))
    m.steps.append({"tool": "create_simulation", "args": {"name": "modal", "analysis": "modal", "material": "", "mesh_size": ms, "refine": False, "modes": 6, "supports": fix}})
    m.steps.append({"tool": "solve_simulation", "args": {"simulation": 0, "probes": [[144.0, 0, 0], [0, 0, 0]]}})
    for k in (1, 2, 3):
        m.steps.append({"tool": "render_simulation", "args": {"simulation": 0, "mode": k, "field": "displacement", "view": "iso", "width": 1400, "height": 900, "path": f"fea-mode{k}.png"}})
    r = run(m.steps, out, "head-modal")
    bad = failures(r)
    if bad: print("FEA failures modal", bad[:5])
    sols = [q.get("result", {"error": q.get("error")}) for q in r if q.get("tool") == "solve_simulation"]
    if not sols or "modes" not in sols[0]: return dict(error=str(sols[:1])[:400])
    s = sols[0]
    return dict(mesh_size=ms, modes=s["modes"], mass_kg=s.get("mass_kg"), probes=s.get("probes"), mesh=s["mesh"])


if __name__ == "__main__":
    folder = os.path.join(ROOT, "cycloidal", f"c{sys.argv[1]}"); out = os.path.join(folder, "fea"); os.makedirs(out, exist_ok=True)
    which = sys.argv[2:] or ["head", "modal"]
    rp = os.path.join(out, "results.json"); res = json.load(open(rp)) if os.path.exists(rp) else {}
    if "head" in which: res["head"] = run_head(folder, out)
    if "modal" in which: res["modal"] = run_modal(folder, out)
    json.dump(res, open(rp, "w"), indent=1)
    def walk(d, pre=""):
        for k, v in d.items():
            if isinstance(v, dict):
                if "magnitude_mm" in v: print(pre + k, round(v["magnitude_mm"]*1000, 2), "um")
                elif k != "mesh": walk(v, pre + k + "/")
            elif k == "modes": print(pre + k, v)
    walk(res)
