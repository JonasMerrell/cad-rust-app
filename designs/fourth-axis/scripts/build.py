#!/usr/bin/env python3
"""DMC2 A axis: parametric model of every design iteration, as rCAD `run` scripts.

    build.py <iteration> [outdir]

Frame: X along the rotation axis (+X from the faceplate toward the tailstock), Y toward the back of the
machine, Z up.  Origin on the axis at the faceplate's front face (the face the vise bolts to).
Every custom part is a stack of prisms along x, y or z, so each is 2.5D work from one or two sides.

Iterations
  1  the gear unit's own crossed-roller bearing is the spindle; one plate; no brake, no tailstock
  2  separate RU85 main bearing in a three-plate bolted block; tailstock
  3  + disc brake with an O-ring piston, home sensor, friction lining
  4  after the simulation of 3: thin-web brake disc, shimmed gear plate, splash ring, home sensor on a printed
     bracket, shorter tail centre
  5  after the adversarial review of 4 (assembly, tool access, how each part is cut): table bolts moved clear of the
     tie bolts, bearing lands under the plates, pocket corners R3.6, tailstock clamp pockets a 1/4 in cutter can make,
     gear turned so its two stop screws clear the motor flange, friction ring moved outside the bearing's nut
     pockets, longer thread engagement for the nose screws, home target moved off a faceplate screw (see REPORT.md)
"""
import sys, os, json, math
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from axlib import Doc, C, R, poly, rrect, slot, bolt_circle, run, failures

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
V = os.path.join(ROOT, "vendor")
STEP_HSS = os.path.join(V, "stepperonline-HSS-25-100-I-D14", "HSS-25-100-I-D14.stp")
STEP_RU = os.path.join(V, "crossed-roller-bearing-RU", "RU85CC0_THK.stp")
STEP_MOTOR = os.path.join(V, "stepperonline-motors", "34E1KIP65-45.STEP")
HSS_BODIES = [[1, 0], [1, 1], [1, 2], [1, 3], [1, 4], [1, 5], [1, 8], [1, 61]]   # everything but balls and small screws

ALU, STEEL = "Aluminium 6061", "Steel"
COL = dict(plate="#b8c2cc", plate2="#a9b4c0", plate3="#c7ced6", base="#8d99a6", rotor="#d9b36c", face="#cfd6dd",
           nose="#b0bac4", piston="#6fa8dc", rubber="#222222", lining="#7a5c3a", vise="#3a3f46", tail="#b8c2cc",
           printed="#e07a3f", work="#e8e2c8")


def pt(r, deg): return (r*math.cos(math.radians(deg)), r*math.sin(math.radians(deg)))
def circ(r, deg, rad): y, z = pt(r, deg); return C(y, z, rad)
def circs(r, degs, rad): return [circ(r, d, rad) for d in degs]


def axis_angle(m):
    """Rotation matrix -> {"axis", "angle"} for rCAD."""
    m = np.asarray(m, float)
    ang = math.acos(max(-1, min(1, (np.trace(m) - 1)/2)))
    if ang < 1e-9: return None
    if abs(ang - math.pi) < 1e-6:
        w, v = np.linalg.eig(m); ax = np.real(v[:, np.argmin(abs(w - 1))])
    else:
        ax = np.array([m[2, 1] - m[1, 2], m[0, 2] - m[2, 0], m[1, 0] - m[0, 1]])/(2*math.sin(ang))
    return {"axis": [float(round(c, 9)) for c in ax], "angle": round(math.degrees(ang), 6)}


def rx(deg): c, s = math.cos(math.radians(deg)), math.sin(math.radians(deg)); return np.array([[1, 0, 0], [0, c, -s], [0, s, c]])
def ry(deg): c, s = math.cos(math.radians(deg)), math.sin(math.radians(deg)); return np.array([[c, 0, s], [0, 1, 0], [-s, 0, c]])


def params(it):
    p = dict(it=it)
    # ---- bought parts (vendor drawings / STEP) ----
    p.update(vise_l=75.0, vise_w=54.0, vise_h=44.0, vise_m6=14.14,
             ru_id=55.0, ru_od=120.0, ru_w=15.0, ru_in_pcd=65.0, ru_out_pcd=105.0, ru_cb=5.4,
             hss_out_to_flange=36.0, hss_flange_t=10.0, hss_flange_r=53.5, hss_body_r=43.0, hss_pcd=96.0,
             hss_m8_pcd=42.0, hss_pilot_in_r=33.5, hss_alpha=(18.0 if it >= 5 else 22.5),
             motor_len=117.8, motor_sq=85.85, motor_holes=69.58, motor_pilot_r=36.5, motor_flange_t=10.0)
    # ---- chosen sizes ----
    p.update(fp_t=12.7, fp_r=59.0, nose_t=11.7, nose_boss=2.0, nose_r=55.0, base_t=12.7,
             W2=78.0, top=64.5, bot=-64.5, chamfer=18.0)
    if it == 1:
        p.update(p1_t=19.05, W2=70.0, bot=-62.0, top=62.0, adapter_t=22.0)
        p["H"] = -p["bot"] + p["base_t"]
        return p
    p.update(p1_t=31.75, c_t=38.1, d_t=31.75, d_gap=(0.5 if it >= 4 else 0.1), brake=(it >= 3), tail=True)
    # brake disc: friction ring from fr_ri to fl_r; inside it the disc is a thin web (iteration 4) that bends to close the gap
    p.update(fr_ri=(57.0 if it >= 5 else 54.0 if it >= 4 else 47.0), web_t=(1.0 if it >= 4 else 2.5), tail_out=(15.0 if it >= 4 else 25.0))
    p.update(rec_d=4.45, lining=0.25, fl_gap=0.15, fl_t=2.5, pis_gap=0.2, pis_t=6.35, pis_proud=1.35,
             fl_r=61.0, rec_r=61.5, gr_ro=61.2, gr_ri=46.5, gr_d=6.0, port_r=57.9, port_deg=20.0)
    p["H"] = -p["bot"] + p["base_t"]
    return p


def stack(p):
    """x positions along the axis, front to back."""
    x = {}
    x["fp_f"] = 0.0; x["fp_r"] = -p["fp_t"]
    x["nose_r"] = x["fp_r"] - p["nose_t"]                    # rear face of the nose flange (outside the boss)
    if p["it"] == 1:
        x["hss_out"] = x["nose_r"]                           # the nose bolts straight onto the gear's output flange
        x["hss_fl_f"] = x["hss_out"] - p["hss_out_to_flange"]
        x["p1_r"] = x["hss_fl_f"]; x["p1_f"] = x["p1_r"] + p["p1_t"]
        x["hss_fl_r"] = x["hss_fl_f"] - p["hss_flange_t"]
        x["ad_r"] = x["hss_fl_r"] - p["adapter_t"]; x["motor_f"] = x["ad_r"]
        x["motor_r"] = x["motor_f"] - p["motor_len"]
        return x
    x["ru_f"] = x["nose_r"] - p["nose_boss"]; x["ru_r"] = x["ru_f"] - p["ru_w"]
    x["p1_f"] = x["ru_f"]; x["p1_r"] = x["p1_f"] - p["p1_t"]
    x["rec_f"] = x["p1_r"] + p["rec_d"]                      # floor of the brake recess in P1's rear face
    x["fl_f"] = x["rec_f"] - p["lining"] - p["fl_gap"]; x["fl_r"] = x["fl_f"] - p["fl_t"]
    x["pis_f"] = x["fl_r"] - p["pis_gap"]; x["pis_r"] = x["pis_f"] - p["pis_t"]
    x["c_f"] = x["p1_r"]; x["c_r"] = x["c_f"] - p["c_t"]
    x["hss_fl_f"] = x["c_r"]; x["hss_out"] = x["hss_fl_f"] + p["hss_out_to_flange"]
    x["hss_fl_r"] = x["hss_fl_f"] - p["hss_flange_t"]
    x["d_f"] = x["c_r"] - p["d_gap"]; x["d_r"] = x["d_f"] - p["d_t"]
    x["motor_f"] = x["d_r"]; x["motor_r"] = x["motor_f"] - p["motor_len"]
    return x


def outline(p, lift=0.0):
    w, t, b, c = p["W2"], p["top"], p["bot"] + lift, p["chamfer"]
    return poly([(-w, b), (w, b), (w, t - c), (w - c, t), (-(w - c), t), (-w, t - c)])


TIES = [(64, 40), (-64, 40), (45, -57), (-45, -57), (68, 0), (-68, 0)]
TIES4 = [(64, 40), (-64, 40), (45, -54), (-45, -54), (68, 0), (-68, 0)]       # iteration 4: lower pair raised so their washers clear the base
UPY = 64.0                                                   # y of the bolts that hold a standing plate down on the base


def upbolt_pockets(part, p, x_face, into, y):
    """Face-milled seat for an M8 bolt that comes up through the base into a standing plate: a slot for the bolt
    and a pocket for a hex nut, cut `into` (+1/-1 along x) from the plate face at x_face."""
    b = p["bot"]; d = 16.0
    a0, a1 = (x_face, x_face + d*into)
    part.cut("x", a0, a1, R(y - 4.5, b - 0.01, y + 4.5, b + 14.5))                 # bolt slot (9 wide)
    part.cut("x", a0, a1, rrect(y - 7.1, b + 13.5, y + 7.1, b + 25.5, 3.6 if p["it"] >= 5 else 3.3))   # nut pocket (M8 hex, 13 AF)


def build(it, outdir):
    p = params(it); x = stack(p); d = Doc(); H = p["H"]; zb = p["bot"]; zt = -H
    ties = TIES4 if it >= 4 else TIES
    hw = d.hardware
    def bom(name, vendor, pn, qty, price, link, note=""): hw.append(dict(name=name, vendor=vendor, pn=pn, qty=qty, price=price, link=link, note=note))

    # =========================================================== custom parts
    # ---- faceplate (carries the vise; comes off the nose with six screws from the front)
    fp = d.part("Faceplate", ALU, COL["face"], stock="1/2 in (12.7) 6061 plate, 125 x 125")
    fp.add("x", x["fp_r"], 0, C(0, 0, p["fp_r"]))
    fp.cut("x", x["fp_r"], 0, circs(50, [30 + 60*i for i in range(6)], 3.3), C(0, 0, 5.5),
           [C(sy*p["vise_m6"], sz*p["vise_m6"], 3.3) for sy in (1, -1) for sz in (1, -1)])
    fp.cut("x", -8.7, 0, circs(50, [30 + 60*i for i in range(6)], 5.5))                  # M6 heads, from the front
    fp.cut("x", x["fp_r"], x["fp_r"] + 6.5, [C(sy*p["vise_m6"], sz*p["vise_m6"], 5.5) for sy in (1, -1) for sz in (1, -1)])
    if it == 3: fp.cut("x", x["fp_r"], x["fp_r"] + 3.3, circ(54.5, -90, 3.25))            # home magnet pocket (rear face)
    if it >= 4: fp.cut("x", -10.0, 0, circ(55.0, -60 if it >= 5 else -90, 3.25))          # home target: a steel dowel pin bonded in, flush with the front face
    fp.done()

    # ---- nose flange
    nose = d.part("Spindle nose", ALU, COL["nose"], stock="5/8 in (15.9) 6061 plate, 115 x 115" if it > 1 else "1/2 in (12.7) 6061 plate")
    nose.add("x", x["nose_r"], x["fp_r"], C(0, 0, p["nose_r"]))
    if it > 1:
        nose.add("x", x["ru_f"], x["nose_r"], C(0, 0, 41.5))                              # boss onto the bearing's inner ring
        nose.cut("x", x["ru_f"], x["fp_r"], circs(p["ru_in_pcd"]/2, [45*i for i in range(8)], 3.25))
        nose.cut("x", x["fp_r"] - (7.2 if it >= 5 else 6.2), x["fp_r"], circs(p["ru_in_pcd"]/2, [45*i for i in range(8)], 5.0))
    else:
        nose.cut("x", x["nose_r"], x["fp_r"], circs(21, [22.5 + 45*i for i in range(8)], 4.5))
        nose.cut("x", x["fp_r"] - 8.6, x["fp_r"], circs(21, [22.5 + 45*i for i in range(8)], 7.0))
    nose.cut("x", x["nose_r"], x["fp_r"], circs(50, [30 + 60*i for i in range(6)], 3.3))
    for i in range(6):                                                                    # open nut slots for the faceplate screws
        a = 30 + 60*i; y0, z0 = pt(49, a); y1, z1 = pt(62, a)
        nose.cut("x", x["nose_r"], x["nose_r"] + 6.5, slot(y0, z0, y1, z1, 5.1))
    nose.done()

    # ---- base plate
    base = d.part("Base plate", ALU, COL["base"], stock="1/2 in (12.7) 6061 plate")
    if it == 1:
        bx0, bx1 = x["p1_r"] - 60, x["p1_f"] + 45
        tb = [(x["p1_f"] + 32.5, 50), (x["p1_f"] + 32.5, -50), (x["p1_f"] + 32.5 - 100, 50), (x["p1_f"] + 32.5 - 100, -50)]
        ups = [(x["p1_r"] + 9.0, 58.0), (x["p1_r"] + 9.0, -58.0)]
        by = 70.0
    else:
        bx0, bx1 = -164.0, (46.0 if it >= 4 else 18.0)
        tb = [(4, 62.5), (4, -62.5), (-146, 62.5), (-146, -62.5)]
        if it >= 5:                                                                      # clear of the tie bolts' ends and of the faceplate
            bx0 = -166.0; tb = [(21, 62.5), (21, -62.5), (-154, 62.5), (-154, -62.5)]
        ups = [(x["p1_r"] + 10, UPY), (x["p1_r"] + 10, -UPY), (x["d_r"] + 10, UPY), (x["d_r"] + 10, -UPY)]
        by = p["W2"]
    base.add("z", zt, zb, rrect(bx0, -by, bx1, by, 6))
    base.cut("z", zt, zb, [C(a, b, 4.5) for a, b in tb], [C(a, b, 4.5) for a, b in ups])
    base.cut("z", zb - 8.5, zb, [C(a, b, 7.25) for a, b in tb])                           # M8 heads, from the top
    base.cut("z", zt, zt + 8.5, [C(a, b, 7.25) for a, b in ups])                          # M8 heads, from below
    if it == 3:
        base.cut("z", zt, zb, C(x["fp_r"]/2, 0, 6.25)); base.cut("z", zt, zt + 5, C(x["fp_r"]/2, 0, 11))   # hall sensor (did not fit: see review)
    if it >= 4:
        base.cut("z", zt, zb, C(30.0, 13.0, 3.25), C(30.0, -13.0, 3.25))                    # sensor bracket screws, from below
        base.cut("z", zt, zt + 6.0, C(30.0, 13.0, 5.0), C(30.0, -13.0, 5.0))
    base.done()

    if it == 1:
        # ---- the one standing plate
        p1 = d.part("Headstock plate", ALU, COL["plate"], stock="3/4 in (19.05) 6061 plate")
        p1.add("x", x["p1_r"], x["p1_f"], outline(p))
        thr = [90 + 9 - 36*i for i in range(10)]
        p1.cut("x", x["p1_r"], x["p1_f"], C(0, 0, 43.5), circs(48, thr, 3.25))
        for a in thr:
            y0, z0 = pt(46.8, a); y1, z1 = pt(49.2, a)
            p1.cut("x", x["p1_f"] - 6.5, x["p1_f"], slot(y0, z0, y1, z1, 4.15))
        for sy in (1, -1): upbolt_pockets(p1, p, x["p1_r"], +1, sy*58.0)
        p1.done()
        ad = d.part("Motor adapter", ALU, COL["plate2"], stock="1 in (25.4) 6061 plate")
        ad.add("x", x["ad_r"], x["hss_fl_r"], C(0, 0, 60))
        ad.cut("x", x["ad_r"], x["hss_fl_r"], C(0, 0, 33.53), circs(48, thr, 5.2),
               circs(48, [81, 9, 189, 261], 3.25), [C(sy*34.79, sz*34.79, 3.3) for sy in (1, -1) for sz in (1, -1)])
        ad.cut("x", x["ad_r"], x["ad_r"] + 2.0, C(0, 0, 36.55))
        for sy in (1, -1):
            for sz in (1, -1):
                ad.cut("x", x["hss_fl_r"] - 9, x["hss_fl_r"], slot(sy*34.0, sz*34.0, sy*35.6, sz*35.6, 5.1))
        ad.done()
    else:
        # ---- P1: bearing plate
        p1 = d.part("P1 bearing plate", ALU, COL["plate"], stock="1-1/4 in (31.75) 6061 plate, 160 x 135")
        p1.add("x", x["p1_r"], x["p1_f"], outline(p))
        p1.cut("x", x["p1_r"], x["p1_f"], C(0, 0, 46.5), [C(a, b, 4.5) for a, b in ties])
        p1.cut("x", x["ru_r"], x["p1_f"], C(0, 0, p["ru_od"]/2 + 0.05))                    # bearing recess, from the front
        floor = x["rec_f"] if p["brake"] else x["p1_r"]
        if p["brake"]: p1.cut("x", x["p1_r"], x["rec_f"], C(0, 0, p["rec_r"]))             # brake recess, from the rear
        p1.cut("x", floor, x["ru_r"], circs(p["ru_out_pcd"]/2, [45*i for i in range(8)], 3.25))
        for i in range(8):
            if it >= 5: y0, z0 = pt(52.5, 45*i - 1.31); y1, z1 = pt(52.5, 45*i + 1.31)      # pockets lie along the circle, inside the friction ring
            else: y0, z0 = pt(51.3, 45*i); y1, z1 = pt(53.7, 45*i)
            p1.cut("x", floor, floor + 7.0, slot(y0, z0, y1, z1, 4.15))
        for sy in (1, -1): upbolt_pockets(p1, p, x["p1_r"], +1, sy*UPY)
        if it >= 5: p1.cut("z", zb - 0.01, zb + 0.2, R(x["p1_r"] + 19.0, -p["W2"] - 1, x["p1_f"] + 1, p["W2"] + 1))   # only the 19 mm cut in one setup bears on the base
        p1.done()

        # ---- C: spacer plate, with the brake cylinder in iteration 3
        cp = d.part("C cylinder plate" if p["brake"] else "C spacer plate", ALU, COL["plate2"], stock="1-1/2 in (38.1) 6061 plate, 160 x 135")
        cp.add("x", x["c_r"], x["c_f"], outline(p, 0.3 if it >= 5 else 0.0))             # iteration 5: C does not stand on the base
        cp.cut("x", x["c_r"], x["c_f"], C(0, 0, 43.5), [C(a, b, 4.5) for a, b in ties])
        if p["brake"]:
            cp.cut("x", x["c_f"] - p["gr_d"], x["c_f"], C(0, 0, p["gr_ro"]), C(0, 0, p["gr_ri"]))
            cp.cut("x", x["c_r"], x["c_f"] - p["gr_d"], circ(p["port_r"], p["port_deg"], 3.25))
        else:
            cp.cut("x", x["c_f"] - 20, x["c_f"], C(0, 0, 50))                               # room for the rotor screws
        cp.done()

        # ---- D: gear clamp and motor plate
        dp = d.part("D motor plate", ALU, COL["plate3"], stock="1-1/4 in (31.75) 6061 plate, 160 x 135")
        dp.add("x", x["d_r"], x["d_f"], outline(p))
        dp.cut("x", x["d_r"], x["d_f"], C(0, 0, 33.53), [C(a, b, 4.5) for a, b in ties],
               [C(sy*34.79, sz*34.79, 3.3) for sy in (1, -1) for sz in (1, -1)],
               circs(48, [90.0, 270.0] if it >= 5 else [94.5, 274.5], 3.25))
        if p["brake"]: dp.cut("x", x["d_r"], x["d_f"], circ(p["port_r"], p["port_deg"], 4.5 if it >= 4 else 3.25))
        dp.cut("x", x["hss_fl_r"] - 0.1, x["d_f"], C(0, 0, 53.7))                           # recess that traps the gear's flange
        dp.cut("x", x["d_r"], x["d_r"] + 2.0, C(0, 0, 36.55))                               # motor pilot
        for sy in (1, -1):
            for sz in (1, -1):
                dp.cut("x", x["hss_fl_r"] - 9.1, x["d_f"], slot(sy*34.0, sz*34.0, sy*35.6, sz*35.6, 5.1))
        for sy in (1, -1): upbolt_pockets(dp, p, x["d_r"], +1, sy*UPY)
        if it >= 5: dp.cut("z", zb - 0.01, zb + 0.2, R(x["d_r"] + 19.0, -p["W2"] - 1, x["d_f"] + 1, p["W2"] + 1))
        dp.done()

        # ---- rotor: joins the gear's output flange to the bearing's inner ring; in iteration 3 its rim is the brake disc
        rot = d.part("Rotor" + (" with brake disc" if p["brake"] else ""), ALU, COL["rotor"], stock="1 in (25.4) 6061 plate, 130 x 130")
        mid_r = x["fl_r"]
        rot.add("x", mid_r, x["ru_r"], C(0, 0, 38))
        rot.add("x", x["hss_out"], mid_r, C(0, 0, 27))
        if p["brake"]:
            rot.add("x", x["fl_r"], x["fl_f"], C(0, 0, p["fl_r"]))
            if p["web_t"] < p["fl_t"]:                                                      # thin web between the hub and the friction ring, cut from the hub side
                rot.cut("x", x["fl_r"] + p["web_t"], x["fl_f"], C(0, 0, p["fr_ri"]), C(0, 0, 38.0))
        rot.cut("x", x["hss_out"], x["ru_r"], circs(21, [22.5 + 45*i for i in range(8)], 4.5), C(0, 0, 5.5))
        rot.cut("x", x["ru_r"] - 8.5, x["ru_r"], circs(21, [22.5 + 45*i for i in range(8)], 7.0))
        rot.cut("x", mid_r, x["ru_r"], circs(32.5, [45*i for i in range(8)], 3.25))
        rot.cut("x", mid_r, mid_r + 2.5, circs(32.5, [45*i for i in range(8)], 5.0))
        rot.done()

        if p["brake"]:
            pis = d.part("Brake piston", ALU, COL["piston"], stock="1/4 in (6.35) 6061 plate, 130 x 130")
            pis.add("x", x["pis_r"], x["pis_f"], C(0, 0, p["gr_ro"] - 0.1), C(0, 0, p["gr_ri"] + 0.2))
            pis.cut("x", x["pis_r"], x["pis_r"] + 3.4, C(0, 0, p["gr_ro"] + 1), C(0, 0, p["gr_ro"] - 2.2))      # O-ring rabbets
            pis.cut("x", x["pis_r"], x["pis_r"] + 3.4, C(0, 0, p["gr_ri"] + 2.2), C(0, 0, p["gr_ri"] - 1))
            if it >= 4: pis.cut("x", x["pis_f"] - 0.6, x["pis_f"], C(0, 0, p["fr_ri"] - 0.5), C(0, 0, p["gr_ri"] - 1))   # push only on the friction ring
            pis.done()
            lin = d.part("Friction lining", "ABS", COL["lining"], kind="cut", stock="P400 silicon carbide paper, bonded")
            lin.add("x", x["rec_f"] - p["lining"], x["rec_f"], C(0, 0, 61.0), C(0, 0, p["fr_ri"]))
            lin.done()
            for nm, rc in (("O-ring 92 x 2.5", p["gr_ri"] + 1.1), ("O-ring 118 x 2.5", p["gr_ro"] - 1.1)):
                o = d.part(nm, "ABS", COL["rubber"], kind="bought")
                o.add("x", x["pis_r"] + 0.6, x["pis_r"] + 2.8, C(0, 0, rc + 1.05), C(0, 0, rc - 1.05))
                o.done(); o.oring = True
            tube = d.part("Air tube stub 6 x 30", "Brass", "#c9a94f", kind="bought")
            ty, tz = pt(p["port_r"], p["port_deg"])
            if it >= 4: tube.add("x", x["d_r"] - 18, x["c_r"] + 15, C(ty, tz, 3.0), C(ty, tz, 2.0))   # bonded into C, passes D with clearance
            else: tube.add("x", x["d_r"] - 18, x["d_r"] + 12, C(ty, tz, 3.0), C(ty, tz, 2.0))
            tube.done()
        if it >= 4:
            # shims between C and D on the six tie bolts: thickness picked at assembly to fill the measured gap (0.5 nominal)
            sh = d.part("Shim washer M8 x 16 x 0.5", STEEL, "#d8d8a0", kind="bought")
            sh.add("x", x["d_f"], x["c_r"], [C(yy, zz, 8.0) for (yy, zz) in ties])
            sh.cut("x", x["d_f"], x["c_r"], [C(yy, zz, 4.2) for (yy, zz) in ties]); sh.done(); sh.n_bodies = 6
            # printed splash ring over the gap between the nose and P1, with a drain at the bottom
            spl = d.part("Splash ring", "ASA", COL["printed"], kind="printed")
            spl.add("x", x["p1_f"], x["p1_f"] + 10.0, C(0, 0, 62.0), C(0, 0, 56.0))
            spl.cut("x", x["p1_f"], x["p1_f"] + 10.0, R(-3.0, -63.0, 3.0, -55.0)); spl.done()
            # home sensor (M8 inductive) in front of the faceplate, on a printed bracket; it sees a steel pin in the faceplate
            brk = d.part("Sensor bracket", "ASA", COL["printed"], kind="printed")
            brk.add("z", zb, zb + 8.0, rrect(12.0, 8.0, 44.0, 18.0, 2), rrect(12.0, -18.0, 44.0, -8.0, 2))
            brk.add("x", 12.0, 20.0, R(-18.0, zb, 18.0, -44.5))
            brk.cut("x", 12.0, 20.0, C(0, -55.0, 4.15))
            brk.cut("z", zb, zb + 8.0, C(30.0, 13.0, 3.2), C(30.0, -13.0, 3.2)); brk.done()
            sen = d.part("Inductive sensor LJ8A3-2-Z/BX", STEEL, "#c0a040", kind="bought")
            sen.add("x", 1.2, 46.2, C(0, -55.0, 4.0)); sen.done()
            ins_ = d.part("Heat-set insert M5", "Brass", "#c9a94f", kind="bought")
            ins_.add("z", zb + 0.2, zb + 7.2, C(30.0, 13.0, 3.2), C(30.0, -13.0, 3.2))
            ins_.cut("z", zb + 0.2, zb + 7.2, C(30.0, 13.0, 2.1), C(30.0, -13.0, 2.1)); ins_.done(); ins_.n_bodies = 2

        # ---- tailstock
        xt = 169.0; tw = 55.0; ttop = 28.0
        tp = d.part("Tailstock plate", ALU, COL["tail"], stock="1 in (25.4) 6061 plate, 115 x 100")
        tp.add("x", xt, xt + 25.4, poly([(-tw, zb), (tw, zb), (tw, ttop - 12), (tw - 12, ttop), (-(tw - 12), ttop), (-tw, ttop - 12)]))
        tp.cut("x", xt, xt + 25.4, C(0, 0, 6.01))
        if it >= 5: tp.cut("x", xt, xt + 25.4, slot(0, -14.75, 0, ttop + 6, 3.25))        # slit with a round end (a 6.35 cutter's own slot, 6.5 wide)
        else: tp.cut("x", xt, xt + 25.4, R(-3.25, -18, 3.25, ttop + 0.01))
        tr = xt + 25.4
        da = 12.7 if it >= 5 else 16.0                                                     # depth of the pinch bolt's axis below the rear face
        tp.cut("x", tr - da - 3.3, tr, R(-18, 12.7, 12.5, 19.3))                           # pinch bolt channel
        if it >= 5:
            tp.cut("x", tr - da - 5.6, tr, rrect(12, 8.0, tw + 12, 24.0, 3.5))             # head pocket, open to the side, flat seat 9 mm wide
            tp.cut("x", tr - da - 5.9, tr, slot(-15.8, 11.8, -15.8, 20.2, 3.3))            # nut pocket: an obround a 6.35 cutter makes
        else:
            tp.cut("x", tr - 16 - 5.6, tr, R(12, 10.4, tw + 0.01, 21.6))                   # head pocket, open to the side
            tp.cut("x", tr - 16 - 5.9, tr, R(-18.2, 10.1, -12.5, 21.9))                    # nut pocket
        d.tail_da = da
        for sy in (1, -1): upbolt_pockets(tp, p, tr, -1, sy*40.0)
        tp.done()
        tb_ = d.part("Tailstock base", ALU, COL["base"], stock="3/4 in (19.05) 6061 plate, 115 x 105")
        tbt = 19.05; tzt = zb - tbt
        tb_.add("z", tzt, zb, rrect(xt - 72, -tw, xt + 30, tw, 6))
        tslots = [(xt - 58, sy*37.5) for sy in (1, -1)] + [(xt - 33, sy*37.5) for sy in (1, -1)]
        tb_.cut("z", tzt, zb, [slot(a - 7, b, a + 7, b, 4.5) for a, b in tslots], [C(tr - 10, sy*40.0, 4.5) for sy in (1, -1)])
        tb_.cut("z", tzt, tzt + 8.5, [C(tr - 10, sy*40.0, 7.25) for sy in (1, -1)])
        tb_.done()
        ctr = d.part("Tail centre (12 mm 90 deg spot drill)", STEEL, "#d0d0d0", kind="bought")
        to = p["tail_out"]
        ctr.add("x", xt - to + 6, xt - to + 75, C(0, 0, 6.0))
        for k in range(6):                                                                 # the 90 degree point, as steps
            ctr.add("x", xt - to + 6 - (k + 1), xt - to + 6 - k, C(0, 0, 6.0 - (k + 0.5)))
        ctr.done()
        p["xt"] = xt; p["tail_tip"] = xt - p["tail_out"]
        d.tail = dict(tp=tp, tb=tb_, ctr=ctr, slots=tslots, tzt=tzt, tr=tr)

    # ---- the vise, drawn from the vendor's two dimension drawings (no vendor CAD exists)
    vs = d.part("Vise XinDian H54-8-55", STEEL, COL["vise"], kind="bought")
    hl, hw_ = p["vise_l"]/2, p["vise_w"]/2
    vs.add("x", 0, 33, rrect(-hl, -hw_, hl, hw_, 2))
    vs.add("x", 33, 44, R(20, -hw_, 39, hw_), R(-39, -hw_, -20, hw_))
    vs.cut("x", 0, 10, [C(sy*14.14, sz*14.14, 2.5) for sy in (1, -1) for sz in (1, -1)], C(0, 0, 4.25),
           [C(sy*20.5, sz*20.5, 2.1) for sy in (1, -1) for sz in (1, -1)], C(18.5, 0, 4.5), C(-18.5, 0, 4.5), C(0, 18.5, 4.5), C(0, -18.5, 4.5))
    vs.cut("x", 14, 30, R(-hl - 1, -8, -hl + 14, 8), R(hl - 14, -8, hl + 1, 8))             # screw openings at the ends
    vs.done()

    # =========================================================== vendor CAD
    s_hss = d.vendor("HSS-25-100-I-D14", STEP_HSS)
    s_mot = d.vendor("34E1KIP65-45", STEP_MOTOR)
    s_ru = d.vendor("RU85", STEP_RU) if it > 1 else None

    # =========================================================== assembly
    G = lambda *a: None
    def station(nm):
        for key, st in (("Faceplate", "face"), ("Spindle nose", "nose"), ("Base plate", "base"), ("P1", "p1"), ("Headstock plate", "p1"), ("C ", "c"), ("D ", "d"),
                        ("Motor adapter", "d"), ("Rotor", "rotor"), ("Brake piston", "brake"), ("Friction", "brake"), ("O-ring", "brake"), ("Air tube", "d"),
                        ("Tailstock base", "tailbase"), ("Tail", "tail"), ("Vise", "vise"), ("Splash", "splash"), ("Shim", "d"), ("Sensor", "base"),
                        ("Inductive", "base"), ("Heat", "base")):
            if nm.startswith(key): return st
        return "p1"
    for part in d.parts:
        part.inst = d.place(part, group=station(part.name))
        for k in range(1, getattr(part, "n_bodies", 1)):                                   # a part drawn as several separate pieces (washers, inserts)
            d.place_body(part.studio, [part.body[0], k], part.name, group=station(part.name))
    alpha = 9.0 if it == 1 else p["hss_alpha"]
    r_hss = axis_angle(rx(alpha) @ ry(-90))
    r_wg = axis_angle(ry(-90))                       # the wave generator's keyway lines up with the motor's key
    for b in HSS_BODIES:
        d.place_body(s_hss, b, "HSS-25 " + str(b[1]), position=(x["hss_out"] - 9.25, 0, 0), rotate=r_wg if b[1] in (8, 61) else r_hss, group="hss")
    r_mot = axis_angle(np.array([[0, 0, 1], [0, -1, 0], [1, 0, 0]]))
    d.place_body(s_mot, [1, 0], "Motor 34E1KIP65-45", position=(x["motor_f"], 0, 0), rotate=r_mot, group="motor")
    if s_ru is not None: d.place_body(s_ru, [1, 0], "RU85UUCC0", position=(x["ru_r"], 0, 0), group="ru")

    g = {"v": "p1"}                                                                   # station of the screws that follow (exploded view)
    def scr(size, L, seat, dirn, name=None): return d.screw(size, L, seat, dirn, name=name, group=g["v"])
    def nut(size, base_, dirn, thin=False): return d.std("ISO 4035" if thin else "ISO 4032", size, base_, dirn, group=g["v"])
    def washer(size, base_, dirn): return d.std("ISO 7089", size, base_, dirn, group=g["v"])

    # faceplate -> nose: 6 x M6 x 16 from the front, nuts in the nose's slots
    for i in range(6):
        y, z = pt(50, 30 + 60*i)
        g["v"] = "face"; scr("M6", 16, (-8.7, y, z), (-1, 0, 0)); g["v"] = "nose"; nut("M6", (x["nose_r"] + 1.3, y, z), (1, 0, 0))
    g["v"] = "face"
    # vise -> faceplate: 4 x M6 x 16 from the rear of the faceplate
    for sy in (1, -1):
        for sz in (1, -1): scr("M6", 16, (x["fp_r"] + 6.5, sy*14.14, sz*14.14), (1, 0, 0))
    g["v"] = "base"
    # base -> table: 4 x M8 x 16
    for a, b in tb: scr("M8", 16, (a, b, zb - 8.5), (0, 0, -1))
    # standing plates -> base: M8 x 25 from below, nuts in the face-milled pockets
    for a, b in ups:
        g["v"] = "base"; scr("M8", 25, (a, b, zt + 8.5), (0, 0, 1)); g["v"] = "p1" if a > -80 else "d"; nut("M8", (a, b, zb + 13.5), (0, 0, 1))

    if it == 1:
        thr = [90 + 9 - 36*i for i in range(10)]
        for a in thr:                                                                     # gear -> plate: 10 x M5 x 30, nuts at the front
            y, z = pt(48, a)
            g["v"] = "hss"; scr("M5", 30, (x["hss_fl_r"], y, z), (1, 0, 0)); g["v"] = "p1"; nut("M5", (x["p1_f"] - 6.5, y, z), (1, 0, 0))
        for a in [81, 9, 189, 261]:                                                       # adapter -> gear's tapped holes
            g["v"] = "d"; y, z = pt(48, a); scr("M5", 30, (x["ad_r"], y, z), (1, 0, 0))
        for i in range(8):                                                                # nose -> gear output: 8 x M8 x 16
            g["v"] = "nose"; y, z = pt(21, 22.5 + 45*i); scr("M8", 16, (x["fp_r"] - 8.6, y, z), (-1, 0, 0))
    else:
        for i in range(8):
            a = 45*i
            y, z = pt(32.5, a)
            g["v"] = "nose"; scr("M5", 12, (x["fp_r"] - (7.2 if it >= 5 else 6.2), y, z), (-1, 0, 0))   # nose -> inner ring, from the front
            g["v"] = "rotor"; scr("M5", 20, (x["fl_r"] + 2.5, y, z), (1, 0, 0))          # rotor -> inner ring, from the rear
            y, z = pt(52.5, a)
            floor = x["rec_f"] if p["brake"] else x["p1_r"]
            L = 20 if p["brake"] else 25
            g["v"] = "ru"; scr("M5", L, (x["ru_f"] - p["ru_cb"], y, z), (-1, 0, 0)); g["v"] = "p1"; nut("M5", (floor + 7.0, y, z), (-1, 0, 0))   # outer ring -> P1
            y, z = pt(21, 22.5 + a)
            g["v"] = "rotor"; scr("M8", 20, (x["ru_r"] - 8.5, y, z), (-1, 0, 0))         # rotor -> gear output, through the bearing bore
        for (y, z) in ties:                                                              # the three plates: 6 x M8 x 120
            g["v"] = "ru"; washer("M8", (x["p1_f"], y, z), (1, 0, 0))
            scr("M8", 120, (x["p1_f"] + 1.6, y, z), (-1, 0, 0))
            g["v"] = "motor"; washer("M8", (x["d_r"], y, z), (-1, 0, 0)); nut("M8", (x["d_r"] - 1.6, y, z), (-1, 0, 0))
        for a in ((90.0, 270.0) if it >= 5 else (94.5, 274.5)):                          # D -> gear's tapped holes (stops it turning)
            g["v"] = "motor"; y, z = pt(48, a); scr("M5", 30, (x["d_r"], y, z), (1, 0, 0))
        t = d.tail; g["v"] = "tail"
        scr("M6", 30, (t["tr"] - d.tail_da, 12, 16), (0, -1, 0)); d.std("ISO 4032", "M6", (t["tr"] - d.tail_da, -12.5, 16), (0, -1, 0), group="tail")   # pinch bolt
        for sy in (1, -1):
            g["v"] = "tailbase"; scr("M8", 30, (t["tr"] - 10, sy*40.0, t["tzt"] + 8.5), (0, 0, 1)); g["v"] = "tail"; nut("M8", (t["tr"] - 10, sy*40.0, zb + 13.5), (0, 0, 1))
        g["v"] = "tailbase"
        for a, b in t["slots"]:
            washer("M8", (a, b, zb), (0, 0, 1)); scr("M8", 30, (a, b, zb + 1.6), (0, 0, -1))
    if it >= 4:
        g["v"] = "base"
        for sy in (1, -1): scr("M5", 12, (30.0, sy*13.0, zt + 6.0), (0, 0, 1))             # sensor bracket, from below into the inserts
        d.std("ISO 4035", "M8", (20.0, 0, -55.0), (1, 0, 0), group="base"); d.std("ISO 4035", "M8", (12.0, 0, -55.0), (-1, 0, 0), group="base")
        hy, hz = pt(55.0, -60 if it >= 5 else -90)
        d.std("ISO 8734", "Ø6", (-10.0, hy, hz), (1, 0, 0), length=10, name="Home target pin 6 x 10", group="face")
    # motor -> its plate: 4 x M6 x 30 from the motor side, nuts under the gear's flange
    for sy in (1, -1):
        for sz in (1, -1):
            g["v"] = "motor"; washer("M6", (x["motor_f"] - p["motor_flange_t"], sy*34.79, sz*34.79), (-1, 0, 0))
            scr("M6", 30, (x["motor_f"] - p["motor_flange_t"] - 1.6, sy*34.79, sz*34.79), (1, 0, 0))
            g["v"] = "d"; nut("M6", (x["hss_fl_r"] - (9.1 if it > 1 else 9.0), sy*34.79, sz*34.79), (1, 0, 0))

    extra = [{"tool": "assembly_info", "args": {"assembly": 0, "interference": True}},
             {"tool": "save_document", "args": {"path": "a-axis.json"}},
             {"tool": "export", "args": {"format": "step", "assembly": 0, "path": "a-axis-assembly.step"}},
             ]
    fit = [[x["motor_r"], -85, -H], [200.0 if it > 1 else 50.0, 85, 70]]
    for view, nm in (("iso_front_right", "iso"), ("iso_back_left", "iso-back"), ("front", "front"), ("top", "top"), ("right", "right")):
        extra.append({"tool": "render_assembly", "args": {"assembly": 0, "view": view, "width": 1800, "height": 1200, "fit_box": fit,
                                                           "path": f"render-{nm}.png"}})
    # per-part STEP and picture
    for part in d.parts:
        stem = part.name.lower().replace(" ", "-").replace("/", "-").replace("(", "").replace(")", "").replace(",", "")
        part.stem = stem
        extra += [{"tool": "switch_part_studio", "args": {"index": part.studio}},
                  {"tool": "export", "args": {"format": "step", "path": f"parts/{stem}.step"}},
                  {"tool": "render", "args": {"view": "iso", "width": 900, "height": 700, "path": f"parts/{stem}.png"}},
                  {"tool": "mass_properties", "args": {}}]
    steps = d.script(extra)
    res = run(steps, outdir, "build")
    meta = dict(params=p, x=x, parts=[dict(name=q.name, stem=q.stem, studio=q.studio, body=q.body, material=q.material, kind=q.kind,
                                             stock=q.stock, inst=q.inst) for q in d.parts], instances=d.inst)
    json.dump(meta, open(os.path.join(outdir, "meta.json"), "w"), indent=1)
    return d, res, meta


if __name__ == "__main__":
    it = int(sys.argv[1]); out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, f"iter{it}")
    d, res, meta = build(it, out)
    bad = failures(res)
    print("steps", len(res), "failed", len(bad))
    for b in bad[:30]: print("  FAIL", b)
    for r in res:
        if r.get("tool") == "assembly_info":
            ai = r["result"]; names = {i["id"]: i["name"] for i in ai["instances"]}
            inter = ai.get("interference", [])
            print("instances", len(ai["instances"]), "interferences", len(inter))
            for i in sorted(inter, key=lambda q: -q["volume_mm3"])[:60]:
                print(f"   {names[i['a']][:34]:34} x {names[i['b']][:34]:34} {i['volume_mm3']:.1f} mm3")
            json.dump(ai, open(os.path.join(out, "assembly_info.json"), "w"))
