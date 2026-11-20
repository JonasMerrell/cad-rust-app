#!/usr/bin/env python3
"""The low A axis (iteration 6 on, and the drive variants) on the real machine model, turned end for end, with the tool setter.

    machine_check6.py <iteration or folder> [X of the faceplate's front face]

The unit is turned 180 degrees about Z: headstock and motor at the right end of the table, work and tailstock toward the left.
The tool setter (40 mm diameter, top 63.8 mm above the table: rcnc's figure) stands with its centre on a table corner: the
back-left one (what rcnc's X -5 / Y 289 imply) and, as a second case, the front-left one.
Tools: 21 mm (shortest, the ball mill), 24 mm (the 1/4 in end mill) and the 40 mm touch probe, each as a 6.35 mm cylinder
under the collet nut of the machine model.
"""
import sys, os, json, math
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import C, run, failures
from build import params, stack, ROOT

axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
MACHINE = os.path.expanduser("~/dmc2/cad/DMC2-cad/DMC2-machine-original.step")
TRAVEL = dict(x=(-123.7, 219.3), y=(-150.7, 83.1), z=(-135.3, 14.7))
SPINDLE = (0.0, -310.0724, 146.5398); TABLE_Z = 11.24
TABLE = dict(x0=-127.2, x1=222.8, y0=-475.3, y1=-225.3)
BASE = list(range(0, 5)); XB = list(range(5, 16)); YB = list(range(16, 21)); ZB = [21, 22, 23]
SETTER_H = 63.8; SETTER_R = 20.0
TOOLS = {"21": 21.0, "24": 24.0, "probe40": 40.0}
R_SWING = 46.5; R_WORK = 45.0


def rot(a_deg):
    """Rz(180) * Rx(a) as axis-angle."""
    a = math.radians(a_deg); c, s = math.cos(a), math.sin(a)
    m = np.array([[-1, 0, 0], [0, -1, 0], [0, 0, 1]]) @ np.array([[1, 0, 0], [0, c, -s], [0, s, c]])
    ang = math.acos(max(-1, min(1, (np.trace(m) - 1)/2)))
    if abs(ang - math.pi) < 1e-6:
        w, v = np.linalg.eig(m); ax = np.real(v[:, np.argmin(abs(w - 1))])
    else:
        ax = np.array([m[2, 1] - m[1, 2], m[0, 2] - m[2, 0], m[1, 0] - m[0, 1]])/(2*math.sin(ang))
    return {"axis": [float(round(q, 9)) for q in ax], "angle": round(math.degrees(ang), 6)}


def main(folder, x_fp=None, y_c=-325.3):
    out = os.path.join(folder, "machine"); os.makedirs(out, exist_ok=True)
    meta = json.load(open(os.path.join(folder, "meta.json"))); p = meta["params"]; x = meta["x"]
    H = p["H"]; z_ax = TABLE_Z + H
    if x_fp is None: x_fp = round(285.0 + x["motor_r"], 1) if p.get("motor_inline", True) else p["x_fp_machine"]
    asm = os.path.join(folder, "a-axis-assembly.step")
    r = run([{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "import_file", "args": {"path": asm}}], out, "list")
    parts = [q for q in r if q.get("tool") == "import_file"][0]["result"]["parts"]
    big = [q for q in parts if q["volume_mm3"] > 2500.0]
    # parts wholly inside the housing cannot meet the machine: left out to keep the cycloidal check quick
    inside = ("Cycloid disc", "Ring pin", "Output sleeves", "Eccentric", "Bearing 6", "Input shaft", "Spacer tube", "Carrier", "Hub spacer", "Shaft seal", "M ring", "G1", "G2")
    if p.get("cyc"): big = [q for q in big if not q["name"].startswith(inside)]
    rot_names = ("Faceplate", "Spindle nose", "Vise", "Rotor", "Hub disc", "Brake disc", "Hub spacer", "Carrier", "Output sleeves")
    tail_tip = p.get("tail_tip", 145.0)

    def cyl(studio_name, axis_plane, off, r_, depth):
        return [{"tool": "new_part_studio", "args": {"name": studio_name}},
                {"tool": "add_feature", "args": {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": axis_plane}, "offset": str(off)}}},
                {"tool": "create_sketch", "args": {"plane": {"kind": "feature_plane", "feature": 1}}},
                {"tool": "sketch_add", "args": {"sketch_id": 2, "items": [C(0, 0, r_)]}},
                {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 2}], "depth": str(depth), "op": "new"}}}]
    s = [{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "import_file", "args": {"path": MACHINE, "name": "DMC2"}},
         {"tool": "new_part_studio", "args": {"name": "A axis"}}, {"tool": "import_file", "args": {"path": asm, "name": "A axis"}}]
    studio = 2; tool_studio = {}
    for nm, L in TOOLS.items():
        s += cyl("Tool " + nm, "Top", -L, 3.175, L); tool_studio[nm] = studio; studio += 1
    s += cyl("Tool setter", "Top", 0, SETTER_R, SETTER_H); st_setter = studio; studio += 1
    s += cyl("Swing vise", "Right", 0.5, R_SWING, 43.5); st_env1 = studio; studio += 1
    s += cyl("Swing part", "Right", 44.5, R_WORK, round(tail_tip - 44.5 - 1.0, 3)); st_env2 = studio; studio += 1
    s.append({"tool": "create_assembly", "args": {"name": "A axis on the DMC2, turned, with the tool setter"}})
    inst = []; nid = 0
    def ins(st, body, name, grp):
        nonlocal nid
        a_ = {"assembly": 0, "studio": st, "body": body, "fixed": True, "name": name}
        if grp in ("axis", "rot", "env"): a_["rotate"] = rot(0.0)          # turned end for end once, here: assembly_move's rotate is relative
        s.append({"tool": "assembly_insert", "args": a_})
        nid += 1; inst.append(dict(id=nid, name=name, grp=grp)); return nid
    for b in BASE: ins(0, [1, b], f"base {b}", "base")
    for b in XB: ins(0, [1, b], f"X {b}", "x")
    for b in YB: ins(0, [1, b], f"table {b}", "y")
    for b in ZB: ins(0, [1, b], f"Z {b}", "z")
    for q in big:
        ins(1, q["body"], "A: " + q["name"][:40], "rot" if any(q["name"].startswith(n) for n in rot_names) else "axis")
    for nm in TOOLS: ins(tool_studio[nm], [3, 0], "tool " + nm, "tool:" + nm)
    ins(st_setter, [3, 0], "tool setter, back-left corner", "setter:back"); ins(st_setter, [3, 0], "tool setter, front-left corner", "setter:front")
    ins(st_env1, [3, 0], "swing envelope: vise", "env"); ins(st_env2, [3, 0], "swing envelope: largest part", "env")
    cur = {"a": 0.0}

    def pose(dx, dy, dz, tool, env=True, a=0.0):
        st = []; ax0 = (x_fp, y_c + dy, z_ax)
        da = a - cur["a"]; cur["a"] = a
        turn = {"axis": [-1.0, 0.0, 0.0], "angle": da} if abs(da) > 1e-9 else None   # the unit's +X points along the machine's -X
        for i in inst:
            g = i["grp"]; rt = None
            if g == "base": continue
            if g == "x": pos = (dx, 0, 0)
            elif g == "z": pos = (dx, 0, dz)
            elif g.startswith("tool:"):
                pos = (SPINDLE[0] + dx, SPINDLE[1], SPINDLE[2] + dz) if g == "tool:" + tool else (SPINDLE[0] + dx, SPINDLE[1], SPINDLE[2] + dz + 3000.0)
            elif g == "y": pos = (0, dy, 0)
            elif g == "setter:back": pos = (TABLE["x0"], TABLE["y1"] + dy, TABLE_Z)
            elif g == "setter:front": pos = (TABLE["x0"], TABLE["y0"] + dy, TABLE_Z)
            elif g == "axis": pos = ax0
            elif g == "rot": pos = ax0; rt = turn
            elif g == "env": pos = ax0 if env else (ax0[0], ax0[1], ax0[2] + 2000.0)
            m = {"assembly": 0, "instance": i["id"], "position": [round(c, 4) for c in pos]}
            if rt: m["rotate"] = rt
            st.append({"tool": "assembly_move", "args": m})
        return st

    dyc = SPINDLE[1] - y_c; zt = TRAVEL["z"][1]
    xs_reach = -120.7                                # rcnc X -5: 3 mm from the left end of X travel
    dy_back = SPINDLE[1] - TABLE["y1"]               # table offset that puts the spindle over the back edge
    dy_front = SPINDLE[1] - TABLE["y0"]
    xj = x_fp - 44.0                                 # jaw tips
    x_tail = x_fp - tail_tip
    poses = []
    def add(name, dx, dy, dz, tool="24", env=True, a=0.0, kind="must be clear"): poses.append(dict(name=name, dx=dx, dy=dy, dz=dz, tool=tool, env=env, a=a, kind=kind))
    def dz_tip(z_tip, L): return z_tip + L - SPINDLE[2]
    # 1. unit against the frame over the Y travel
    for dy in (TRAVEL["y"][0], -75.0, 0.0, TRAVEL["y"][1]):
        add(f"Y sweep, head parked left, dy {dy:+.1f}", TRAVEL["x"][0], dy, zt)
    # 2. touch-off on the setter at the back-left corner, every tool
    for nm, L in TOOLS.items():
        add(f"touching the setter (back-left corner), {nm} mm tool", xs_reach, dy_back, dz_tip(TABLE_Z + SETTER_H, L), tool=nm)
    # 3. the route: Z fully up, along the setter's Y line from the right end to the setter; then the tool-change point
    for nm in TOOLS:
        for dx in (219.3, x_fp + 60.0, x_fp - 22.0, 47.8, xj - 50.0, x_tail, xs_reach):
            add(f"route along the back edge at Z top, X {dx:+.1f}, {nm} mm tool", dx, dy_back, zt, tool=nm)
    # 4. from the work to the back line: Z fully up over the axis and half way back, along the work zone
    for nm in TOOLS:
        for dx in (xj - 4.0, xj - 50.0, x_tail + 8.0):
            add(f"over the work at Z top, X {dx:+.1f}, on the axis, {nm} mm tool", dx, dyc, zt, tool=nm, kind=("must be clear" if nm != "probe40" else "probe over a 90 mm part"))
            add(f"over the work at Z top, X {dx:+.1f}, leaving toward the back, {nm} mm tool", dx, dyc - 60.0, zt, tool=nm)
    # 5. Z fully up over the headstock, the vise and the motor with each tool
    for nm in TOOLS:
        for dx, what in ((x_fp - 22.0, "vise"), (x_fp + 12.0, "faceplate and nose"), (x_fp + 60.0, "plates"), (min(219.3, x_fp + 180.0), "motor")):
            for off in (0.0, 40.0):
                add(f"Z top over the {what}, {off:.0f} mm off the axis, {nm} mm tool", dx, dyc - off, zt, tool=nm,
                    kind=("must be clear" if nm != "probe40" else "probe over the headstock (not needed)"))
    # 6. cutting
    for dx, nm_ in ((xj - 4.0, "4 mm past the jaws"), (xj - 50.0, "50 mm past the jaws"), (x_tail + 8.0, "8 mm before the centre")):
        add(f"top of a 90 mm part, {nm_}", dx, dyc, dz_tip(z_ax + R_WORK + 0.3, 24.0))
    for a in (0.0, 90.0):
        add(f"top of a 20 mm part 14 mm past the jaws, A {a:.0f}", xj - 14.0, dyc, dz_tip(z_ax + 10.3, 24.0), env=False, a=a)
        add(f"beside a 20 mm part at centre height, 50 mm past the jaws, A {a:.0f}", xj - 50.0, dyc + (10 + 3.175 + 0.3), dz_tip(z_ax - 3.0, 24.0), env=False, a=a)
    # 7. the front-left corner case (outside the CAD's Y travel: geometry only)
    for nm, L in TOOLS.items():
        add(f"touching the setter (front-left corner), {nm} mm tool: beyond the CAD's Y travel", xs_reach, dy_front, dz_tip(TABLE_Z + SETTER_H, L), tool=nm, kind="front-left case")
        add(f"route along the front edge at Z top, X {x_tail:+.1f}, {nm} mm tool: beyond the CAD's Y travel", x_tail, dy_front, zt, tool=nm, kind="front-left case")
    shots = ("touching the setter (back-left corner), 24", "top of a 90 mm part, 50", "route along the back edge at Z top, X +47.8, probe40")
    for i, q in enumerate(poses):
        s += pose(q["dx"], q["dy"], q["dz"], q["tool"], q["env"], q["a"])
        s.append({"tool": "assembly_info", "args": {"assembly": 0, "interference": True}})
        if q["name"].startswith(shots):
            for view in ("iso_front_right", "front", "top"):
                s.append({"tool": "render_assembly", "args": {"assembly": 0, "view": view, "width": 1800, "height": 1200, "path": f"pose{i:02d}-{view}.png"}})
    s += pose(xj - 50.0, dyc, dz_tip(z_ax + R_WORK + 0.3, 24.0), "24", False, 0.0)
    s += [{"tool": "save_document", "args": {"path": "on-machine.json"}}]
    for view, nm_ in (("iso_front_right", "iso"), ("front", "front"), ("top", "top")):
        s.append({"tool": "render_assembly", "args": {"assembly": 0, "view": view, "width": 2000, "height": 1400, "path": f"on-machine-{nm_}.png"}})
    s.append({"tool": "render_assembly", "args": {"assembly": 0, "view": "iso_front_right", "width": 2000, "height": 1400, "path": "on-machine-close.png",
                                                 "fit_box": [[-210, y_c + dyc - 150, 0], [300, y_c + dyc + 120, 260]]}})
    r = run(s, out, "machine")
    bad = failures(r)
    if bad: print("failures", bad[:8])
    infos = [q["result"] for q in r if q.get("tool") == "assembly_info" and "result" in q]
    names = {i["id"]: i for i in inst}; res = []
    for q, ai in zip(poses, infos):
        hits = []
        for h in ai.get("interference", []):
            a_, b_ = names[h["a"]], names[h["b"]]; ga, gb = a_["grp"].split(":")[0], b_["grp"].split(":")[0]
            mach = {"base", "x", "z", "tool"}; unit = {"axis", "rot", "env"}; tbl = {"y", "setter"}
            # the touch-off itself: the tool meets the setter it is touching, and only that
            if {ga, gb} == {"tool", "setter"} and "touching" in q["name"] and h["volume_mm3"] < 5.0: continue
            if (ga in mach and gb in unit) or (gb in mach and ga in unit) or (ga in tbl and gb in mach) or (gb in tbl and ga in mach) or \
               (ga == "setter" and gb in unit) or (gb == "setter" and ga in unit):
                if "setter:front" in (a_["grp"], b_["grp"]) and q["kind"] != "front-left case": continue      # the other corner's setter is a stand-in for the second case only
                if "setter:back" in (a_["grp"], b_["grp"]) and q["kind"] == "front-left case": continue
                if h["volume_mm3"] > 0.5: hits.append(dict(a=a_["name"], b=b_["name"], volume_mm3=h["volume_mm3"]))
        res.append(dict(q, hits=hits))
    json.dump(dict(placement=dict(x_faceplate=x_fp, y_axis=y_c, z_axis=z_ax, table_offset_tool_over_axis=dyc, turned=True, jaw_tips_x=xj, tail_centre_x=x_tail,
                                  motor_rear_x=x_fp - x["motor_r"], setter_back=[TABLE["x0"], TABLE["y1"]], setter_front=[TABLE["x0"], TABLE["y0"]]),
                   poses=res, instances=inst), open(os.path.join(out, "results.json"), "w"), indent=1)
    for q in res:
        print(("HIT  " if q["hits"] else "clear") + f" [{q['kind']}] {q['name']}  (dx {q['dx']:+.1f} dy {q['dy']:+.1f} dz {q['dz']:+.1f})")
        for h in q["hits"][:5]: print(f"        {h['a']} x {h['b']}: {h['volume_mm3']:.0f} mm3")


if __name__ == "__main__":
    a = sys.argv[1]; folder = a if os.path.isdir(a) else os.path.join(ROOT, f"iter{a}")
    main(folder, float(sys.argv[2]) if len(sys.argv) > 2 else None)
