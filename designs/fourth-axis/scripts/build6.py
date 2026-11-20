#!/usr/bin/env python3
"""Iterations 6 and later: the low headstock that leaves the tool setter working.

Same frame as build.py (X along the axis from the faceplate toward the tailstock, origin on the axis at the faceplate's
front face). On the machine the whole unit is turned end for end (machine_check.py does that): headstock and motor at
the right end of the table, tailstock and work toward the left where the tool setter is.

What differs from iteration 5:
  - RU66 main bearing (95 mm outside diameter) instead of RU85, so the plates can be round-topped at R56 and the
    centre height drops from 77.2 to 63.9 mm: the whole unit is 120 mm tall, under a retracted 24 mm tool (126 mm);
  - steel (1018 cold-rolled flat) for the faceplate, the nose, two identical hub discs and the base plate;
  - spindle stack clamped from the front: four M6 through nose, bearing bore and hub A into nuts held between hub A
    and the brake disc; hub A, disc and hub B are bolted to the reducer's output with its eight M8 on the bench;
  - brake piston in its own plate (C1) in front of the reducer body, so its ring can run from R34 to R52.6.
"""
import sys, os, json, math
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from axlib import Doc, C, R, poly, rrect, slot, run, failures

HERE = os.path.dirname(os.path.abspath(__file__)); ROOT = os.path.dirname(HERE); V = os.path.join(ROOT, "vendor")
STEP_HSS = os.path.join(V, "stepperonline-HSS-25-100-I-D14", "HSS-25-100-I-D14.stp")
STEP_MOTOR = os.path.join(V, "stepperonline-motors", "34E1KIP65-45.STEP")
HSS_BODIES = [[1, 0], [1, 1], [1, 2], [1, 3], [1, 4], [1, 5], [1, 8], [1, 61]]
ALU, STEEL = "Aluminium 6061", "Steel"
COL = dict(plate="#b8c2cc", plate2="#a9b4c0", plate3="#c7ced6", plate4="#9fb0c2", base="#6f7780", rotor="#d9b36c", steel="#7d858e", face="#8a929b",
           piston="#6fa8dc", rubber="#222222", lining="#7a5c3a", vise="#3a3f46", tail="#b8c2cc", printed="#e07a3f")


def pt(r, deg): return (r*math.cos(math.radians(deg)), r*math.sin(math.radians(deg)))
def circ(r, deg, rad): y, z = pt(r, deg); return C(y, z, rad)
def circs(r, degs, rad): return [circ(r, d, rad) for d in degs]
def rx(deg): c, s = math.cos(math.radians(deg)), math.sin(math.radians(deg)); return np.array([[1, 0, 0], [0, c, -s], [0, s, c]])
def ry(deg): c, s = math.cos(math.radians(deg)), math.sin(math.radians(deg)); return np.array([[c, 0, s], [0, 1, 0], [-s, 0, c]])


def axis_angle(m):
    m = np.asarray(m, float); ang = math.acos(max(-1, min(1, (np.trace(m) - 1)/2)))
    if ang < 1e-9: return None
    if abs(ang - math.pi) < 1e-6:
        w, v = np.linalg.eig(m); ax = np.real(v[:, np.argmin(abs(w - 1))])
    else:
        ax = np.array([m[2, 1] - m[1, 2], m[0, 2] - m[2, 0], m[1, 0] - m[0, 1]])/(2*math.sin(ang))
    return {"axis": [float(round(c, 9)) for c in ax], "angle": round(math.degrees(ang), 6)}


def params6(it):
    p = dict(it=it, low=True, brake=True, tail=True)
    p.update(vise_l=75.0, vise_w=54.0, vise_h=44.0, vise_m6=14.14)
    # THK RU66 (dimension table, no vendor CAD): 35 x 95 x 15, inner 8 x M4 on 45, outer 8 x 4.5 with 8 x 4.4 counterbores on 83
    p.update(ru_id=35.0, ru_od=95.0, ru_w=15.0, ru_in_pcd=45.0, ru_out_pcd=83.0, ru_cb=4.4, ru_dp=66.0)
    p.update(hss_out_to_flange=36.0, hss_flange_t=10.0, hss_flange_r=53.5, hss_body_r=43.0, hss_alpha=0.0,
             motor_len=117.8, motor_flange_t=10.0)
    p.update(fp_t=9.53, fp_r=50.0, nose_t=12.7, nose_boss=1.0, nose_r=48.0, base_t=7.94, hub_t=12.7, hub_r=30.0,
             W2=75.0, top=56.0, bot=-56.0, r_top=56.0, shoulder=10.0)
    p.update(p1_t=31.75, c1_t=12.7, d_t=25.4, d_gap=0.6, rec_d=4.45, lining=0.25, fl_gap=0.15, fl_t=3.175, web_t=1.0, pis_gap=0.2, pis_t=6.35,
             fl_r=52.0, fr_ri=46.0, rec_r=52.6, gr_ro=52.6, gr_ri=34.0, web_ri=30.5, tail_out=15.0, tail_base_t=19.05)
    p["H"] = -p["bot"] + p["base_t"]
    p["tail_bot"] = -p["H"] + p["tail_base_t"]
    return p


def stack6(p):
    x = {}
    x["fp_f"] = 0.0; x["fp_r"] = -p["fp_t"]
    x["nose_r"] = x["fp_r"] - (p["nose_t"] - p["nose_boss"])        # rear face of the nose outside the boss
    x["ru_f"] = x["fp_r"] - p["nose_t"]; x["ru_r"] = x["ru_f"] - p["ru_w"]
    x["p1_f"] = x["ru_f"]; x["p1_r"] = x["p1_f"] - p["p1_t"]
    x["rec_f"] = x["p1_r"] + p["rec_d"]                               # rear face of P1's web = where the lining is bonded
    x["hubA_r"] = x["ru_r"] - p["hub_t"]
    x["fl_f"] = x["hubA_r"]; x["fl_r"] = x["fl_f"] - p["fl_t"]          # brake disc, clamped between the two hubs
    x["hubB_r"] = x["fl_r"] - p["hub_t"]
    x["hss_out"] = x["hubB_r"]
    x["hss_fl_f"] = x["hss_out"] - p["hss_out_to_flange"]; x["hss_fl_r"] = x["hss_fl_f"] - p["hss_flange_t"]
    x["pis_f"] = x["fl_r"] - p["pis_gap"]; x["pis_r"] = x["pis_f"] - p["pis_t"]
    x["c_f"] = x["p1_r"]; x["c1_r"] = x["c_f"] - p["c1_t"]; x["c_r"] = x["hss_fl_f"]
    x["gr_floor"] = x["pis_r"] - 1.0                                   # 1 mm of air space behind the piston
    x["d_f"] = x["c_r"] - p["d_gap"]; x["d_r"] = x["d_f"] - p["d_t"]
    x["motor_f"] = x["d_r"]; x["motor_r"] = x["motor_f"] - p["motor_len"]
    return x


def outline6(p, lift=0.0):
    """Round top of radius r_top with sloping shoulders down to the full width: every corner is convex."""
    w, b, r, s = p["W2"], p["bot"] + lift, p["r_top"], p["shoulder"]
    d = math.hypot(w, s); a = math.atan2(s, w) + math.acos(r/d)
    ty, tz = r*math.cos(a), r*math.sin(a)
    L = lambda a_, b_: {"type": "line", "from": [round(a_[0], 4), round(a_[1], 4)], "to": [round(b_[0], 4), round(b_[1], 4)]}
    return [L((-w, b), (w, b)), L((w, b), (w, s)), L((w, s), (ty, tz)),
            {"type": "arc", "center": [0, 0], "start": [round(ty, 4), round(tz, 4)], "end": [round(-ty, 4), round(tz, 4)]},
            L((-ty, tz), (-w, s)), L((-w, s), (-w, b))]


TIES6 = [(65, 4), (-65, 4), (65, -22), (-65, -22)]
UPY6 = 64.0
PORT6 = (-64.0, -9.0)


def upbolt6(part, bot, x_face, into, y):
    d = 16.0
    part.cut("x", x_face, x_face + d*into, R(y - 4.5, bot - 0.01, y + 4.5, bot + 14.5))
    part.cut("x", x_face, x_face + d*into, rrect(y - 7.1, bot + 13.5, y + 7.1, bot + 25.5, 3.6))


def build6(it, outdir):
    p = params6(it); x = stack6(p); d = Doc(); H = p["H"]; zb = p["bot"]; zt = -H
    M8A = [45*i for i in range(8)]                 # reducer output bolts, in the assembly
    M6A = [22.5 + 90*i for i in range(4)]          # the four clamp screws through the bearing bore, on R9
    FPA = [60, 90, 120, 240, 270, 300]             # faceplate screws on R42, outside the vise (its 75 mm length lies along y)

    # ---------------------------------------------------------------- rotating parts
    fp = d.part("Faceplate (steel)", STEEL, COL["face"], stock="1018 cold-rolled flat 3/8 x 4 in (9.53 x 101.6), 105 long")
    fp.add("x", x["fp_r"], 0, C(0, 0, p["fp_r"]))
    fp.cut("x", x["fp_r"], 0, circs(42, FPA, 3.3), [C(sy*p["vise_m6"], sz*p["vise_m6"], 3.3) for sy in (1, -1) for sz in (1, -1)], C(0, 0, 5.5))
    fp.cut("x", -6.5, 0, circs(42, FPA, 5.5))
    fp.cut("x", x["fp_r"], x["fp_r"] + 6.5, [C(sy*p["vise_m6"], sz*p["vise_m6"], 5.5) for sy in (1, -1) for sz in (1, -1)])
    fp.cut("x", x["fp_r"], 0, R(48.6, -20.0, 55.0, 20.0))                                 # home flat on the rim (1.4 deep), toward +y at A = 0
    fp.done()

    nose = d.part("Spindle nose (steel)", STEEL, COL["steel"], stock="1018 cold-rolled flat 1/2 x 4 in (12.7 x 101.6), 100 long")
    nose.add("x", x["nose_r"], x["fp_r"], C(0, 0, p["nose_r"]))
    nose.add("x", x["ru_f"], x["nose_r"], C(0, 0, 29.5))                                  # boss onto the bearing's inner ring
    nose.cut("x", x["ru_f"], x["fp_r"], circs(9, M6A, 3.3), circs(42, FPA, 3.3))
    nose.cut("x", x["fp_r"] - 6.5, x["fp_r"], circs(9, M6A, 5.5))
    for a in FPA:                                                                         # open nut slots for the faceplate screws
        y0, z0 = pt(41.0, a); y1, z1 = pt(55.0, a)
        nose.cut("x", x["nose_r"], x["nose_r"] + 5.6, slot(y0, z0, y1, z1, 5.1))
    nose.done()

    hub = d.part("Hub disc (steel)", STEEL, COL["steel"], stock="1018 cold-rolled flat 1/2 x 2-1/2 in (12.7 x 63.5), 65 long each")
    def hub_at(part, xr, xf):
        part.add("x", xr, xf, C(0, 0, p["hub_r"]))
        part.cut("x", xr, xf, circs(21, M8A, 4.5), circs(9, M6A, 3.3))
        part.cut("x", xf - 8.6, xf, circs(21, M8A, 7.0))                                  # M8 heads, front
        for a in M6A:                                                                     # M6 nut pockets, rear, lying along the circle
            y0, z0 = pt(9, a - 6.0); y1, z1 = pt(9, a + 6.0)
            part.cut("x", xr, xr + 7.0, slot(y0, z0, y1, z1, 5.1))
    hub_at(hub, x["hubA_r"], x["ru_r"]); hub.done()
    hub2 = d.part("Hub disc (steel) B", STEEL, COL["steel"], stock="same part as the other hub disc")
    hub_at(hub2, x["hubB_r"], x["fl_r"]); hub2.done()

    disc = d.part("Brake disc", ALU, COL["rotor"], stock="1/8 in (3.18) 6061 sheet, 110 x 110")
    disc.add("x", x["fl_r"], x["fl_f"], C(0, 0, p["fl_r"]))
    disc.cut("x", x["fl_r"], x["fl_f"], circs(21, M8A, 4.5), circs(9, M6A, 3.5))
    disc.cut("x", x["fl_r"], x["fl_f"] - p["web_t"], C(0, 0, p["fr_ri"] - 1.0), C(0, 0, p["web_ri"]))      # the thin web, cut from the rear
    disc.done()

    # ---------------------------------------------------------------- standing plates
    def ties_and_ups(part, xr, xf, ups_face=None):
        part.cut("x", xr, xf, [C(a, b, 4.5) for a, b in TIES6])
        if ups_face is not None:
            for sy in (1, -1): upbolt6(part, zb, ups_face, +1, sy*UPY6)
            part.cut("z", zb - 0.01, zb + 0.2, R(ups_face + 19.0, -p["W2"] - 1, xf + 1, p["W2"] + 1))  # only the 19 mm land bears

    p1 = d.part("P1 bearing plate", ALU, COL["plate"], stock="1-1/4 in (31.75) 6061 plate, 155 x 115")
    p1.add("x", x["p1_r"], x["p1_f"], outline6(p))
    p1.cut("x", x["p1_r"], x["p1_f"], C(0, 0, 37.0))
    p1.cut("x", x["ru_r"], x["p1_f"], C(0, 0, p["ru_od"]/2 + 0.05))
    p1.cut("x", x["p1_r"], x["rec_f"], C(0, 0, p["rec_r"]))
    p1.cut("x", x["rec_f"], x["ru_r"], circs(p["ru_out_pcd"]/2, M8A, 3.25))
    for a in M8A:                                                                         # M4 nut pockets in the web's rear face, along the circle
        y0, z0 = pt(41.65, a - 1.2); y1, z1 = pt(41.65, a + 1.2)
        p1.cut("x", x["rec_f"], x["rec_f"] + 6.0, slot(y0, z0, y1, z1, 3.65))
    ties_and_ups(p1, x["p1_r"], x["p1_f"], x["p1_r"])
    p1.done()

    c1 = d.part("C1 piston plate", ALU, COL["plate2"], stock="1/2 in (12.7) 6061 plate, 155 x 115")
    c1.add("x", x["c1_r"], x["c_f"], outline6(p, 0.3))
    c1.cut("x", x["c1_r"], x["c_f"], C(0, 0, p["hub_r"] + 1.0))
    c1.cut("x", x["gr_floor"], x["c_f"], C(0, 0, p["gr_ro"]), C(0, 0, p["gr_ri"]))
    c1.cut("x", x["c1_r"], x["gr_floor"], C(-48.5, PORT6[1], 3.25))                        # air into the groove
    c1.cut("x", x["c1_r"], x["c1_r"] + 1.0, C(0, 0, 43.5))                                 # clears the front of the reducer's body
    ties_and_ups(c1, x["c1_r"], x["c_f"])
    c1.done()

    c2 = d.part("C2 spacer plate", ALU, COL["plate4"], stock="1-1/2 in (38.1) 6061 plate, 155 x 115, faced to %.2f" % (x["c1_r"] - x["c_r"]))
    c2.add("x", x["c_r"], x["c1_r"], outline6(p, 0.3))
    c2.cut("x", x["c_r"], x["c1_r"], C(0, 0, 43.5), C(PORT6[0], PORT6[1], 3.25))
    c2.cut("x", x["c1_r"] - 3.0, x["c1_r"], slot(-48.5, PORT6[1], PORT6[0], PORT6[1], 3.25))   # air channel in the front face, from the groove hole out to the port
    ties_and_ups(c2, x["c_r"], x["c1_r"])
    c2.done()

    dp = d.part("D motor plate", ALU, COL["plate3"], stock="1 in (25.4) 6061 plate, 155 x 115")
    dp.add("x", x["d_r"], x["d_f"], outline6(p))
    dp.cut("x", x["d_r"], x["d_f"], C(0, 0, 33.53), [C(sy*34.79, sz*34.79, 3.3) for sy in (1, -1) for sz in (1, -1)], circs(48, [0.0, 180.0], 3.25),
           C(PORT6[0], PORT6[1], 4.5))
    dp.cut("x", x["hss_fl_r"] - 0.1, x["d_f"], C(0, 0, 53.7))
    dp.cut("x", x["d_r"], x["d_r"] + 2.0, C(0, 0, 36.55))
    for sy in (1, -1):
        for sz in (1, -1): dp.cut("x", x["hss_fl_r"] - 9.1, x["d_f"], slot(sy*34.0, sz*34.0, sy*35.6, sz*35.6, 5.1))
    ties_and_ups(dp, x["d_r"], x["d_f"], x["d_r"])
    dp.done()

    base = d.part("Base plate (steel)", STEEL, COL["base"], stock="1018 cold-rolled flat 5/16 x 6 in (7.94 x 152.4), 215 long")
    bx0, bx1 = -168.0, 44.0
    tb = [(21, 62.5), (21, -62.5), (-154, 62.5), (-154, -62.5)]
    ups = [(x["p1_r"] + 10, UPY6), (x["p1_r"] + 10, -UPY6), (x["d_r"] + 10, UPY6), (x["d_r"] + 10, -UPY6)]
    base.add("z", zt, zb, rrect(bx0, -p["W2"], bx1, p["W2"], 6))
    BRK = [(6.0, 68.0), (-12.0, 68.0)]                                                      # sensor bracket screws
    base.cut("z", zt, zb, [C(a, b, 4.5) for a, b in tb], [C(a, b, 4.5) for a, b in ups], [C(a, b, 3.25) for a, b in BRK])
    for a, b in ups: base.cut("z", zt, zt + 4.6, C(a, b, 8.2))                              # countersinks for the M8 that come up (cut with the 90 degree mill; drawn as recesses)
    base.done()

    pis = d.part("Brake piston", ALU, COL["piston"], stock="1/4 in (6.35) 6061 plate, 110 x 110")
    pis.add("x", x["pis_r"], x["pis_f"], C(0, 0, p["gr_ro"] - 0.1), C(0, 0, p["gr_ri"] + 0.2))
    pis.cut("x", x["pis_r"], x["pis_r"] + 3.4, C(0, 0, p["gr_ro"] + 1), C(0, 0, p["gr_ro"] - 2.2))
    pis.cut("x", x["pis_r"], x["pis_r"] + 3.4, C(0, 0, p["gr_ri"] + 2.2), C(0, 0, p["gr_ri"] - 1))
    pis.cut("x", x["pis_f"] - 0.6, x["pis_f"], C(0, 0, p["fr_ri"] - 0.5), C(0, 0, p["gr_ri"] - 1))
    pis.done()
    lin = d.part("Friction lining", "ABS", COL["lining"], kind="cut", stock="P400 silicon carbide paper, bonded")
    lin.add("x", x["rec_f"] - p["lining"], x["rec_f"], C(0, 0, p["fl_r"]), C(0, 0, p["fr_ri"])); lin.done()
    for nm, rc in (("O-ring 68 x 2.5", p["gr_ri"] + 1.1), ("O-ring 100 x 2.5", p["gr_ro"] - 1.1)):
        o = d.part(nm, "ABS", COL["rubber"], kind="bought")
        o.add("x", x["pis_r"] + 0.6, x["pis_r"] + 2.8, C(0, 0, rc + 1.05), C(0, 0, rc - 1.05)); o.done()
    tube = d.part("Air tube stub 6 x 60", "Brass", "#c9a94f", kind="bought")
    tube.add("x", x["d_r"] - 18, x["c_r"] + 15, C(PORT6[0], PORT6[1], 3.0), C(PORT6[0], PORT6[1], 2.0)); tube.done()
    sh = d.part("Shim washer M8 x 16 x 0.6", STEEL, "#d8d8a0", kind="bought")
    sh.add("x", x["d_f"], x["c_r"], [C(yy, zz, 8.0) for (yy, zz) in TIES6]); sh.cut("x", x["d_f"], x["c_r"], [C(yy, zz, 4.2) for (yy, zz) in TIES6]); sh.done(); sh.n_bodies = 4
    spl = d.part("Splash ring", "ASA", COL["printed"], kind="printed")
    spl.add("x", x["p1_f"], x["p1_f"] + 10.0, C(0, 0, 54.5), C(0, 0, 49.0)); spl.cut("x", x["p1_f"], x["p1_f"] + 10.0, R(-3.0, -55.5, 3.0, -48.0)); spl.done()

    # bought RU66, drawn from THK's dimension table
    ru = d.part("RU66 crossed roller bearing", STEEL, "#9aa3ad", kind="bought")
    ru.add("x", x["ru_r"], x["ru_f"], C(0, 0, 47.5), C(0, 0, 17.5))
    ru.cut("x", x["ru_r"], x["ru_f"], circs(41.5, M8A, 2.25), circs(22.5, [22.5 + 45*i for i in range(8)], 1.65))
    ru.cut("x", x["ru_f"] - 4.4, x["ru_f"], circs(41.5, M8A, 4.0)); ru.done()

    # home sensor: M8 inductive, looking along -y at the steel faceplate's rim from the +y side; it switches off at the notch
    sx = -p["fp_t"]/2
    brk = d.part("Sensor bracket", "ASA", COL["printed"], kind="printed")
    brk.add("z", zb, zb + 8.0, rrect(-20.5, 58.0, 12.0, 75.0, 2))
    brk.add("y", 58.0, 66.0, R(sx - 12.0, zb, sx + 12.0, 12.0))
    brk.cut("y", 58.0, 66.0, C(sx, 0.0, 4.15))
    brk.cut("z", zb, zb + 8.0, [C(a, b, 3.2) for a, b in BRK]); brk.done()
    sen = d.part("Inductive sensor LJ8A3-2-Z/BX", STEEL, "#c0a040", kind="bought")
    sen.add("y", 51.2, 96.2, C(sx, 0.0, 4.0)); sen.done()
    ins_ = d.part("Heat-set insert M5", "Brass", "#c9a94f", kind="bought")
    ins_.add("z", zb + 0.2, zb + 7.2, [C(a, b, 3.2) for a, b in BRK]); ins_.cut("z", zb + 0.2, zb + 7.2, [C(a, b, 2.1) for a, b in BRK]); ins_.done(); ins_.n_bodies = 2

    # ---------------------------------------------------------------- tailstock (as iteration 5, on its own 19 mm base)
    xt = 160.0; tw = 55.0; ttop = 28.0; tb_ = p["tail_bot"]
    tp = d.part("Tailstock plate", ALU, COL["tail"], stock="1 in (25.4) 6061 plate, 115 x 80")
    tp.add("x", xt, xt + 25.4, poly([(-tw, tb_), (tw, tb_), (tw, ttop - 12), (tw - 12, ttop), (-(tw - 12), ttop), (-tw, ttop - 12)]))
    tp.cut("x", xt, xt + 25.4, C(0, 0, 6.01)); tp.cut("x", xt, xt + 25.4, slot(0, -14.75, 0, ttop + 6, 3.25))
    tr = xt + 25.4; da = 12.7
    tp.cut("x", tr - da - 3.3, tr, R(-18, 12.7, 12.5, 19.3))
    tp.cut("x", tr - da - 5.6, tr, rrect(12, 8.0, tw + 12, 24.0, 3.5))
    tp.cut("x", tr - da - 5.9, tr, slot(-15.8, 11.8, -15.8, 20.2, 3.3))
    for sy in (1, -1): upbolt6(tp, tb_, tr, -1, sy*40.0)
    tp.done()
    tbp = d.part("Tailstock base", ALU, COL["plate3"], stock="3/4 in (19.05) 6061 plate, 115 x 105")
    tbp.add("z", zt, tb_, rrect(xt - 72, -tw, xt + 30, tw, 6))
    tslots = [(xt - 58, sy*37.5) for sy in (1, -1)] + [(xt - 33, sy*37.5) for sy in (1, -1)]
    tbp.cut("z", zt, tb_, [slot(a - 7, b, a + 7, b, 4.5) for a, b in tslots], [C(tr - 10, sy*40.0, 4.5) for sy in (1, -1)])
    tbp.cut("z", zt, zt + 8.5, [C(tr - 10, sy*40.0, 7.25) for sy in (1, -1)]); tbp.done()
    to = p["tail_out"]
    ctr = d.part("Tail centre (12 mm 90 deg spot drill)", STEEL, "#d0d0d0", kind="bought")
    ctr.add("x", xt - to + 6, xt - to + 75, C(0, 0, 6.0))
    for k in range(6): ctr.add("x", xt - to + 6 - (k + 1), xt - to + 6 - k, C(0, 0, 6.0 - (k + 0.5)))
    ctr.done()
    p["xt"] = xt; p["tail_tip"] = xt - to

    # ---------------------------------------------------------------- vise (from the vendor's drawings)
    vs = d.part("Vise XinDian H54-8-55", STEEL, COL["vise"], kind="bought")
    hl, hw_ = p["vise_l"]/2, p["vise_w"]/2
    vs.add("x", 0, 33, rrect(-hl, -hw_, hl, hw_, 2)); vs.add("x", 33, 44, R(20, -hw_, 39, hw_), R(-39, -hw_, -20, hw_))
    vs.cut("x", 0, 10, [C(sy*14.14, sz*14.14, 2.5) for sy in (1, -1) for sz in (1, -1)], C(0, 0, 4.25),
           [C(sy*20.5, sz*20.5, 2.1) for sy in (1, -1) for sz in (1, -1)], C(18.5, 0, 4.5), C(-18.5, 0, 4.5), C(0, 18.5, 4.5), C(0, -18.5, 4.5))
    vs.cut("x", 14, 30, R(-hl - 1, -8, -hl + 14, 8), R(hl - 14, -8, hl + 1, 8)); vs.done()

    s_hss = d.vendor("HSS-25-100-I-D14", STEP_HSS); s_mot = d.vendor("34E1KIP65-45", STEP_MOTOR)

    # ---------------------------------------------------------------- assembly
    def station(nm):
        for key, st in (("Faceplate", "face"), ("Spindle nose", "nose"), ("Base plate", "base"), ("P1", "p1"), ("C1", "c"), ("C2", "c2"), ("D ", "d"),
                        ("Hub disc (steel) B", "hubb"), ("Hub disc", "huba"), ("Brake disc", "rotor"), ("Brake piston", "brake"), ("Friction", "p1"), ("O-ring", "brake"),
                        ("Air tube", "d"), ("Tailstock base", "tailbase"), ("Tail", "tail"), ("Vise", "vise"), ("Splash", "splash"), ("Shim", "d"),
                        ("Sensor", "base"), ("Inductive", "base"), ("Heat", "base"), ("RU66", "ru")):
            if nm.startswith(key): return st
        return "p1"
    for part in d.parts:
        part.inst = d.place(part, group=station(part.name))
        for k in range(1, getattr(part, "n_bodies", 1)): d.place_body(part.studio, [part.body[0], k], part.name, group=station(part.name))
    r_hss = axis_angle(rx(p["hss_alpha"]) @ ry(-90))
    r_wg = axis_angle(ry(-90))
    for b in HSS_BODIES:
        d.place_body(s_hss, b, "HSS-25 " + str(b[1]), position=(x["hss_out"] - 9.25, 0, 0), rotate=r_wg if b[1] in (8, 61) else r_hss, group="hss")
    d.place_body(s_mot, [1, 0], "Motor 34E1KIP65-45", position=(x["motor_f"], 0, 0), rotate=axis_angle(np.array([[0, 0, 1], [0, -1, 0], [1, 0, 0]])), group="motor")

    g = {"v": "p1"}
    def scr(size, L, seat, dirn, std="ISO 4762"): return d.screw(size, L, seat, dirn, std=std, group=g["v"])
    def nut(size, base_, dirn): return d.std("ISO 4032", size, base_, dirn, group=g["v"])
    def washer(size, base_, dirn): return d.std("ISO 7089", size, base_, dirn, group=g["v"])
    for a in FPA:                                                                       # faceplate -> nose: 6 x M6 x 14, nuts in the nose
        y, z = pt(42, a)
        g["v"] = "face"; scr("M6", 14, (-6.5, y, z), (-1, 0, 0)); g["v"] = "nose"; nut("M6", (x["nose_r"] + 5.6, y, z), (-1, 0, 0))
    g["v"] = "face"
    for sy in (1, -1):
        for sz in (1, -1): scr("M6", 12, (x["fp_r"] + 6.5, sy*14.14, sz*14.14), (1, 0, 0))   # vise, from behind the faceplate
    for a in M6A:                                                                       # nose + bearing + hub A: 4 x M6 x 35 from the front
        y, z = pt(9, a)
        g["v"] = "nose"; scr("M6", 35, (x["fp_r"] - 6.5, y, z), (-1, 0, 0)); g["v"] = "huba"; nut("M6", (x["hubA_r"] + 7.0, y, z), (-1, 0, 0))
    for a in M8A:
        y, z = pt(21, a); g["v"] = "huba"; scr("M8", 30, (x["ru_r"] - 8.6, y, z), (-1, 0, 0))    # hub A + disc + hub B -> reducer output
        y, z = pt(41.5, a); g["v"] = "ru"; scr("M4", 20, (x["ru_f"] - p["ru_cb"], y, z), (-1, 0, 0))
        g["v"] = "p1"; nut("M4", (x["rec_f"] + 6.0, y, z), (-1, 0, 0))
    for (y, z) in TIES6:
        g["v"] = "ru"; washer("M8", (x["p1_f"], y, z), (1, 0, 0)); scr("M8", 120, (x["p1_f"] + 1.6, y, z), (-1, 0, 0))
        g["v"] = "motor"; washer("M8", (x["d_r"], y, z), (-1, 0, 0)); nut("M8", (x["d_r"] - 1.6, y, z), (-1, 0, 0))
    for a in (0.0, 180.0):                                                              # two of the reducer's tapped holes (they sit at 72 - 36k degrees)
        y, z = pt(48, a); g["v"] = "motor"; scr("M5", 25, (x["d_r"], y, z), (1, 0, 0))
    for sy in (1, -1):
        for sz in (1, -1):
            g["v"] = "motor"; washer("M6", (x["motor_f"] - p["motor_flange_t"], sy*34.79, sz*34.79), (-1, 0, 0))
            scr("M6", 25, (x["motor_f"] - p["motor_flange_t"] - 1.6, sy*34.79, sz*34.79), (1, 0, 0))
            g["v"] = "d"; nut("M6", (x["hss_fl_r"] - 9.1, sy*34.79, sz*34.79), (1, 0, 0))
    g["v"] = "base"
    for a, b in tb: washer("M8", (a, b, zb), (0, 0, 1)); scr("M8", 20, (a, b, zb + 1.6), (0, 0, -1))
    for a, b in ups:
        g["v"] = "base"; scr("M8", 30, (a, b, zt), (0, 0, 1), std="ISO 10642"); g["v"] = "p1" if a > -80 else "d"; nut("M8", (a, b, zb + 13.5), (0, 0, 1))
    g["v"] = "base"
    for (a, b) in BRK: scr("M5", 12, (a, b, zt), (0, 0, 1), std="ISO 10642")
    d.std("ISO 4035", "M8", (sx, 66.0, 0.0), (0, 1, 0), group="base"); d.std("ISO 4035", "M8", (sx, 58.0, 0.0), (0, -1, 0), group="base")
    g["v"] = "tail"
    scr("M6", 30, (tr - da, 12, 16), (0, -1, 0)); d.std("ISO 4032", "M6", (tr - da, -12.5, 16), (0, -1, 0), group="tail")
    for sy in (1, -1):
        g["v"] = "tailbase"; scr("M8", 30, (tr - 10, sy*40.0, zt + 8.5), (0, 0, 1)); g["v"] = "tail"; nut("M8", (tr - 10, sy*40.0, tb_ + 13.5), (0, 0, 1))
    g["v"] = "tailbase"
    for a, b in tslots: washer("M8", (a, b, tb_), (0, 0, 1)); scr("M8", 30, (a, b, tb_ + 1.6), (0, 0, -1))

    extra = [{"tool": "assembly_info", "args": {"assembly": 0, "interference": True}},
             {"tool": "save_document", "args": {"path": "a-axis.json"}},
             {"tool": "export", "args": {"format": "step", "assembly": 0, "path": "a-axis-assembly.step"}}]
    fit = [[x["motor_r"], -85, -H], [200.0, 85, 60]]
    for view, nm in (("iso_front_right", "iso"), ("iso_back_left", "iso-back"), ("front", "front"), ("top", "top"), ("right", "right")):
        extra.append({"tool": "render_assembly", "args": {"assembly": 0, "view": view, "width": 1800, "height": 1200, "fit_box": fit, "path": f"render-{nm}.png"}})
    for part in d.parts:
        stem = part.name.lower().replace(" ", "-").replace("/", "-").replace("(", "").replace(")", "").replace(",", "")
        part.stem = stem
        extra += [{"tool": "switch_part_studio", "args": {"index": part.studio}},
                  {"tool": "export", "args": {"format": "step", "path": f"parts/{stem}.step"}},
                  {"tool": "render", "args": {"view": "iso", "width": 900, "height": 700, "path": f"parts/{stem}.png"}},
                  {"tool": "mass_properties", "args": {}}]
    res = run(d.script(extra), outdir, "build")
    meta = dict(params=p, x=x, parts=[dict(name=q.name, stem=q.stem, studio=q.studio, body=q.body, material=q.material, kind=q.kind, stock=q.stock, inst=q.inst)
                                       for q in d.parts], instances=d.inst)
    json.dump(meta, open(os.path.join(outdir, "meta.json"), "w"), indent=1)
    return d, res, meta
