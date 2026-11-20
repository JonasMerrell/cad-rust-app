#!/usr/bin/env python3
"""The A axis on the real machine model: rCAD assembly interference at chosen axis positions.

    machine_check.py <iteration>      -> iter<N>/machine/ (results.json, pictures, on-machine.json rCAD document)

The machine is Jonas's "Simplified DMC2" STEP (the trunnion he does not own left out). Its X, Y and Z bodies are
moved by the axis offsets (same convention as the rCAD machine file: mm from the modelled position), the A axis
rides with the table, and a 6.35 x 24 tool hangs from the collet nut.  Besides the real parts a "swing envelope"
(cylinder of the vise's swing radius, and of the largest workpiece) stands in for every A angle at once.
"""
import sys, os, json, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import C, run, failures
from build import params, stack, ROOT

axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
MACHINE = os.path.expanduser("~/dmc2/cad/DMC2-cad/DMC2-machine-original.step")
TRAVEL = dict(x=(-123.7, 219.3), y=(-150.7, 83.1), z=(-135.3, 14.7))
SPINDLE = (0.0, -310.0724, 146.5398); TABLE_Z = 11.24
BASE = list(range(0, 5)); XB = list(range(5, 16)); YB = list(range(16, 21)); ZB = [21, 22, 23]
X_FP, Y_C = 51.0, -325.3                  # where the faceplate's front face and the axis sit on the table (machine coordinates)
R_SWING = 46.5; R_WORK = 45.0


def main(it):
    out = os.path.join(ROOT, f"iter{it}", "machine"); os.makedirs(out, exist_ok=True)
    p = params(it); x = stack(p); H = p["H"]; z_ax = TABLE_Z + H
    asm = os.path.join(ROOT, f"iter{it}", "a-axis-assembly.step")
    # pass 1: what is in the exported assembly
    r = run([{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "import_file", "args": {"path": asm}}], out, "list")
    parts = [q for q in r if q.get("tool") == "import_file"][0]["result"]["parts"]
    big = [q for q in parts if q["volume_mm3"] > 2500.0]
    rot_names = ("Faceplate", "Spindle nose", "Vise", "Rotor")
    tail_tip = p.get("tail_tip", 150.0)

    s = [{"tool": "new_document", "args": {"units": "mm"}},
         {"tool": "import_file", "args": {"path": MACHINE, "name": "DMC2"}},
         {"tool": "new_part_studio", "args": {"name": "A axis"}},
         {"tool": "import_file", "args": {"path": asm, "name": "A axis"}},
         {"tool": "new_part_studio", "args": {"name": "Tool"}},
         {"tool": "add_feature", "args": {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": "Top"}, "offset": "-24"}}},
         {"tool": "create_sketch", "args": {"plane": {"kind": "feature_plane", "feature": 1}}},
         {"tool": "sketch_add", "args": {"sketch_id": 2, "items": [C(0, 0, 3.175)]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 2}], "depth": "24", "op": "new"}}},
         {"tool": "new_part_studio", "args": {"name": "Swing envelope"}},
         {"tool": "add_feature", "args": {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": "Right"}, "offset": "0.5"}}},
         {"tool": "create_sketch", "args": {"plane": {"kind": "feature_plane", "feature": 1}}},
         {"tool": "sketch_add", "args": {"sketch_id": 2, "items": [C(0, 0, R_SWING)]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 2}], "depth": "43.5", "op": "new"}}},
         {"tool": "add_feature", "args": {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": "Right"}, "offset": "44.5"}}},
         {"tool": "create_sketch", "args": {"plane": {"kind": "feature_plane", "feature": 4}}},
         {"tool": "sketch_add", "args": {"sketch_id": 5, "items": [C(0, 0, R_WORK)]}},
         {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 5}], "depth": str(round(tail_tip - 44.5 - 1.0, 3)), "op": "new"}}},
         {"tool": "create_assembly", "args": {"name": "A axis on the DMC2"}}]
    inst = []; nid = 0
    def ins(studio, body, name, grp):
        nonlocal nid
        s.append({"tool": "assembly_insert", "args": {"assembly": 0, "studio": studio, "body": body, "fixed": True, "name": name}})
        nid += 1; inst.append(dict(id=nid, name=name, grp=grp)); return nid
    for b in BASE: ins(0, [1, b], f"base {b}", "base")
    for b in XB: ins(0, [1, b], f"X {b}", "x")
    for b in YB: ins(0, [1, b], f"table {b}", "y")
    for b in ZB: ins(0, [1, b], f"Z {b}", "z")
    for q in big:
        grp = "rot" if any(q["name"].startswith(n) for n in rot_names) else "axis"
        ins(1, q["body"], "A: " + q["name"][:40], grp)
    ins(2, [3, 0], "tool 6.35 x 24", "tool")
    ins(3, [3, 0], "swing envelope: vise", "env"); ins(3, [6, 0], "swing envelope: largest part", "env")

    def pose(dx, dy, dz, env=True, a=0.0):
        st = []
        ax0 = (X_FP, Y_C + dy, z_ax)
        for i in inst:
            g = i["grp"]
            if g == "base": continue
            if g == "x": pos = (dx, 0, 0); rot = None
            elif g == "z": pos = (dx, 0, dz); rot = None
            elif g == "tool": pos = (SPINDLE[0] + dx, SPINDLE[1], SPINDLE[2] + dz); rot = None
            elif g == "y": pos = (0, dy, 0); rot = None
            elif g == "axis": pos = ax0; rot = None
            elif g == "rot": pos = ax0; rot = {"axis": [1, 0, 0], "angle": a} if abs(a) > 1e-9 else {"axis": [1, 0, 0], "angle": 0.0}
            elif g == "env": pos = ax0 if env else (ax0[0], ax0[1], ax0[2] + 2000.0); rot = None
            m = {"assembly": 0, "instance": i["id"], "position": [round(c, 4) for c in pos]}
            if rot: m["rotate"] = rot
            st.append({"tool": "assembly_move", "args": m})
        return st

    dyc = SPINDLE[1] - Y_C                         # table offset that puts the tool over the axis
    poses = []
    zt = TRAVEL["z"][1]
    def add(name, dx, dy, dz, env=True, a=0.0, kind="must be clear"): poses.append(dict(name=name, dx=dx, dy=dy, dz=dz, env=env, a=a, kind=kind))
    # A: the unit against the frame over the whole Y travel, head parked right and left at Z top
    for dy in (TRAVEL["y"][0], -75.0, 0.0, TRAVEL["y"][1]):
        add(f"Y sweep, head right, dy {dy:+.1f}", TRAVEL["x"][1], dy, zt)
        add(f"Y sweep, head left, dy {dy:+.1f}", TRAVEL["x"][0], dy, zt)
    # B: Z fully up with a 24 mm tool: over the motor, over the vise and over the work it must be clear; over the plates it cannot be
    for dx, kind in ((-123.7, "must be clear"), (-60.0, "keep-out: the plates stand higher than the retracted tool"), (0.0, "keep-out: the plates stand higher than the retracted tool"),
                     (X_FP - 6.0, "keep-out: the faceplate stands higher than the retracted tool"), (X_FP + 22.0, "must be clear"), (120.0, "must be clear"), (219.3, "must be clear")):
        for off in (-R_SWING, 0.0, R_SWING):
            add(f"Z top, tool at X {dx:+.1f}, {off:+.0f} mm from the axis", dx, dyc - off, zt, kind=kind)
    # route to the left end with a tool in: behind the unit (table forward), then over the motor
    add("Z top, tool behind the unit over the plates (route to the left end)", 0.0, dyc - (p["W2"] + 8.0), zt)
    add("Z top, tool behind the unit, left end", -123.7, dyc - (p["W2"] + 8.0), zt)
    # C: cutting positions
    def dz_tip(z_tip): return z_tip + 24.0 - SPINDLE[2]
    xj = X_FP + 44.0
    for xx, nm in ((xj + 4.0, "4 mm past the jaws"), (xj + 60.0, "60 mm past the jaws"), (min(X_FP + tail_tip - 8.0, 219.3), "8 mm before the centre")):
        add(f"top of a 90 mm part, {nm}", xx, dyc, dz_tip(z_ax + R_WORK + 0.3))
    # how far down the side of a part the tool gets before the collet nut or the spindle body meets the part (envelope cylinder hidden;
    # a test cylinder is not modelled: the vise at A = 0 and the machine are what is checked, the part-versus-nut limit is calculated in REPORT.md)
    for a in (0.0, 90.0):
        add(f"top of a 20 mm part 4 mm past the jaws, A {a:.0f}", xj + 4.0, dyc, dz_tip(z_ax + 10.3), env=False, a=a, kind="reach")
        add(f"top of a 20 mm part 14 mm past the jaws, A {a:.0f}", xj + 14.0, dyc, dz_tip(z_ax + 10.3), env=False, a=a)
        add(f"beside a 20 mm part at centre height, 14 mm past the jaws, tool in front, A {a:.0f}", xj + 14.0, dyc + (10 + 3.175 + 0.3), dz_tip(z_ax - 3.0), env=False, a=a, kind="reach")
        add(f"beside a 20 mm part at centre height, 14 mm past the jaws, tool behind, A {a:.0f}", xj + 14.0, dyc - (10 + 3.175 + 0.3), dz_tip(z_ax - 3.0), env=False, a=a, kind="reach")
        add(f"beside a 20 mm part at centre height, 50 mm past the jaws, tool in front, A {a:.0f}", xj + 50.0, dyc + (10 + 3.175 + 0.3), dz_tip(z_ax - 3.0), env=False, a=a)
        add(f"tool on the jaw tops (facing the jaw step), A {a:.0f}", xj - 3.0, dyc, dz_tip(z_ax + 12.0 if a == 0 else z_ax + 40.0), env=False, a=a, kind="reach")
    # E: what must NOT be done: tool down beside the headstock
    add("Z fully down over the headstock (expected to collide)", X_FP - 60.0, dyc, TRAVEL["z"][0], kind="expected collision")
    for i, q in enumerate(poses):
        s += pose(q["dx"], q["dy"], q["dz"], q["env"], q["a"])
        s.append({"tool": "assembly_info", "args": {"assembly": 0, "interference": True}})
        if q["name"].startswith(("top of a 90 mm part, 60", "Y sweep, head right, dy +83", "beside a 20 mm part at centre height, 50 mm past the jaws, tool in front, A 0")):
            for view in ("iso_front_right", "front", "right"):
                s.append({"tool": "render_assembly", "args": {"assembly": 0, "view": view, "width": 1800, "height": 1200, "path": f"pose{i:02d}-{view}.png"}})
    # leave the document in a typical cutting pose
    s += pose(xj + 60.0, dyc, dz_tip(z_ax + R_WORK + 0.3), False, 0.0)
    s += [{"tool": "save_document", "args": {"path": "on-machine.json"}},
          {"tool": "render_assembly", "args": {"assembly": 0, "view": "iso_front_right", "width": 2000, "height": 1400, "path": "on-machine-iso.png"}},
          {"tool": "render_assembly", "args": {"assembly": 0, "view": "front", "width": 2000, "height": 1200, "path": "on-machine-front.png"}},
          {"tool": "render_assembly", "args": {"assembly": 0, "view": "top", "width": 2000, "height": 1400, "path": "on-machine-top.png"}},
          {"tool": "render_assembly", "args": {"assembly": 0, "view": "iso_front_right", "width": 2000, "height": 1400, "path": "on-machine-close.png",
                                               "fit_box": [[-210, Y_C + dyc - 150, 0], [300, Y_C + dyc + 120, 260]]}}]
    r = run(s, out, "machine")
    bad = failures(r)
    if bad: print("failures", bad[:8])
    infos = [q["result"] for q in r if q.get("tool") == "assembly_info" and "result" in q]
    names = {i["id"]: i for i in inst}
    res = []
    for q, ai in zip(poses, infos):
        hits = []
        for h in ai.get("interference", []):
            a_, b_ = names[h["a"]], names[h["b"]]
            ga, gb = a_["grp"], b_["grp"]
            mach = {"base", "x", "z", "tool"}; unit = {"axis", "rot", "env"}
            if (ga in mach and gb in unit) or (gb in mach and ga in unit) or ({ga, gb} == {"y", "x"}) or ({ga, gb} == {"y", "z"}) or ({ga, gb} == {"y", "tool"}):
                if h["volume_mm3"] > 0.5: hits.append(dict(a=a_["name"], b=b_["name"], volume_mm3=h["volume_mm3"]))
        res.append(dict(q, hits=hits))
    json.dump(dict(placement=dict(x_faceplate=X_FP, y_axis=Y_C, z_axis=z_ax, table_offset_tool_over_axis=dyc), poses=res, instances=inst),
              open(os.path.join(out, "results.json"), "w"), indent=1)
    for q in res:
        print(("HIT  " if q["hits"] else "clear") + f" [{q['kind']}] {q['name']}  (dx {q['dx']:+.1f} dy {q['dy']:+.1f} dz {q['dz']:+.1f})")
        for h in q["hits"][:6]: print(f"        {h['a']} x {h['b']}: {h['volume_mm3']:.0f} mm3")


if __name__ == "__main__":
    main(int(sys.argv[1]))
