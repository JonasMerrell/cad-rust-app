#!/usr/bin/env python3
"""The cycloidal A axis: iteration 6's low headstock (RU66 main bearing, brake, steel faceplate, 120 mm tall) with a
self-made two-disc cycloidal reducer in place of the bought strain-wave unit, driven by a NEMA 23 through a 3:1 belt.

    buildc.py <n>      -> cycloidal/c<n>/

Frame as build6.py: X along the axis, origin on the axis at the faceplate's front face, the reducer toward -X.

The reducer, front to back:
  hub spacer (steel, runs in the lip seal that keeps grease out of the brake)
  carrier 1 (steel)        bolted to hub A / brake disc / hub spacer by six M6; holds the input shaft's front bearing
  pin plate G1             24 holes for the ring pins (8 mm dowels on a 88 mm circle)
  disc A, disc B           identical, cut stacked, one turned half a turn; each on its own 6904 bearing on an eccentric bush
  ring spacer M            between the pin plates, around the discs
  pin plate G2
  carrier 2 (steel)        clamped to carrier 1 by six M6 through six hardened sleeves (2 needle-bearing inner rings each)
  rear plate D             input shaft's rear bearing, the motor on an ear, belt behind it under a printed guard

Zero backlash: three sleeves (A) sit in pockets of carrier 1 and drive disc A; the other three (B) sit in pockets of
carrier 2 and drive disc B. Each disc has three working holes and three oversize ones. With the six screws slack,
carrier 2 is twisted against carrier 1 (spanner holes, through windows in D) until disc A bears on one flank and disc B
on the other, then the screws are tightened through the access holes in D.
"""
import sys, os, json, math
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import axlib
from axlib import Doc, C, R, poly, rrect, slot, run, failures
from build6 import (pt, circ, circs, ry, axis_angle, params6, outline6, TIES6, UPY6, PORT6, upbolt6, COL, ALU, STEEL, V, ROOT)
import cycloid

STEP_M23 = os.path.join(V, "stepperonline-motors", "23E1KIP65-20.STEP")
COLC = dict(disc="#c98f4a", disc2="#d9a760", car="#7d858e", pin="#e6e6e6", brg="#9aa3ad", bush="#b06a3a", belt="#1c1c1c", pulley="#b0b6bd", shaft="#d0d0d0")


def paramsc(it):
    p = params6(6); p["it"] = f"c{it}"; p["cyc"] = True
    p.update(c1_t=19.05, hubB_t=19.05, car_r=38.0, car1_t=12.7, car2_t=9.53, g_t=12.7, d_t=15.88, disc_t=6.35, sleeve_l=21.0,
             N=cycloid.N, Rp=cycloid.R, rp=cycloid.RP, e=cycloid.E, RO=29.0, rq=5.0, clear=0.03, r_over=7.25, brg_r=18.5, bush_w=9.0,
             pin_l=40.0, ym=107.76, zm=-26.0, belt_c=110.85, z1=20, z2=60, belt="GT3 345-3MGT-9 (115 teeth)", motor_len=110.8, motor_sq=60.25,
             motor_inline=False, x_fp_machine=39.4, pins_g="Aluminium 6061")
    if it >= 2:                                   # c2: all-steel mesh (steel pin plates) and a sprung preload (B sleeves on flexure blades in carrier 2)
        p.update(pins_g=STEEL, sprung=True, blade_t=2.0, preload_nm=6.0, span_r=20.0)
    return p


def stackc(p):
    x = {}
    x["fp_f"] = 0.0; x["fp_r"] = -p["fp_t"]
    x["nose_r"] = x["fp_r"] - (p["nose_t"] - p["nose_boss"])
    x["ru_f"] = x["fp_r"] - p["nose_t"]; x["ru_r"] = x["ru_f"] - p["ru_w"]
    x["p1_f"] = x["ru_f"]; x["p1_r"] = x["p1_f"] - p["p1_t"]
    x["rec_f"] = x["p1_r"] + p["rec_d"]
    x["hubA_r"] = x["ru_r"] - p["hub_t"]
    x["fl_f"] = x["hubA_r"]; x["fl_r"] = x["fl_f"] - p["fl_t"]
    x["hubB_r"] = x["fl_r"] - p["hubB_t"]
    x["pis_f"] = x["fl_r"] - p["pis_gap"]; x["pis_r"] = x["pis_f"] - p["pis_t"]
    x["c_f"] = x["p1_r"]; x["c1_r"] = x["c_f"] - p["c1_t"]
    x["gr_floor"] = x["pis_r"] - 1.0
    x["seal_f"] = x["c1_r"] + 8.5                                   # floor of the seal pocket in C1's rear face
    x["car1_f"] = x["hubB_r"]; x["car1_r"] = x["car1_f"] - p["car1_t"]
    x["g1_f"] = x["c1_r"]; x["g1_r"] = x["g1_f"] - p["g_t"]
    x["bush_f"] = x["car1_r"] - 0.5                                 # 0.5 mm shim between the front bearing and bush A
    x["bushA_r"] = x["bush_f"] - p["bush_w"]; x["bushB_r"] = x["bushA_r"] - p["bush_w"]
    off = (p["bush_w"] - p["disc_t"])/2
    x["dA_f"] = x["bush_f"] - off; x["dA_r"] = x["dA_f"] - p["disc_t"]
    x["dB_f"] = x["bushA_r"] - off; x["dB_r"] = x["dB_f"] - p["disc_t"]
    x["slA_f"] = x["car1_r"] + 2.5                                   # sleeves A: floor of their pockets in carrier 1
    x["car2_f"] = x["slA_f"] - p["sleeve_l"]; x["car2_r"] = x["car2_f"] - p["car2_t"]
    x["slB_f"] = x["car1_r"] - (0.5 if p.get("sprung") else 0.0)     # sleeves B: sprung, they stop 0.5 mm short of carrier 1
    x["slB_r"] = x["slB_f"] - p["sleeve_l"]                          # floor of their pockets in carrier 2
    x["m_f"] = x["g1_r"]; x["m_r"] = x["dB_r"] - 0.95
    x["g2_f"] = x["m_r"]; x["g2_r"] = x["g2_f"] - p["g_t"]
    x["d_f"] = x["g2_r"]; x["d_r"] = x["d_f"] - p["d_t"]
    x["d_rec"] = x["car2_r"] - 6.0 - 0.7                             # floor of D's recess over the screw heads
    x["brg_f"] = x["d_r"] + 8.0                                      # rear shaft bearing, in a pocket from D's rear
    x["web_f"] = x["d_r"] + 6.56                                     # the motor ear: 6.56 mm web, the motor's flange on its front
    x["belt_f"] = x["d_r"] - 10.5; x["belt_r"] = x["belt_f"] - 9.0
    x["guard_r"] = x["d_r"] - 29.84
    x["motor_f"] = x["web_f"]; x["motor_r"] = -164.0                 # motor_r: the unit's rear end (the base plate), for the machine check
    x["motor_end"] = x["web_f"] + p["motor_len"]
    return x


def disc_points(p, flip):
    """Outline of a disc in the assembly (y, z): disc A's centre at +e, disc B (flip) turned half a turn about its own centre, centre at -e."""
    pr = cycloid.profile(n_pts=23*100, offset=-p["clear"])
    if flip: return [(-a - p["e"], -b) for a, b in pr]
    return [(a + p["e"], b) for a, b in pr]


def hull(c1, r1, c2, r2, n=40):
    """Convex outline around two circles (the belt), as points."""
    (y1, z1), (y2, z2) = c1, c2; d = math.hypot(y2 - y1, z2 - z1); th = math.atan2(z2 - z1, y2 - y1); b = math.acos((r1 - r2)/d)
    pts = []
    for k in range(n + 1):
        a = th + b + (2*math.pi - 2*b)*k/n; pts.append((y1 + r1*math.cos(a), z1 + r1*math.sin(a)))
    for k in range(n + 1):
        a = th - b + (2*b)*k/n; pts.append((y2 + r2*math.cos(a), z2 + r2*math.sin(a)))
    return pts


def buildc(it, outdir):
    p = paramsc(it); x = stackc(p); d = Doc(); H = p["H"]; zb = p["bot"]; zt = -H; e = p["e"]
    M6H = [30 + 60*i for i in range(6)]            # hub screws on R21
    M6A = [22.5 + 90*i for i in range(4)]          # the four clamp screws through the bearing bore, on R9
    FPA = [60, 90, 120, 240, 270, 300]
    SA = [0, 120, 240]; SB = [60, 180, 300]        # sleeves A (seated in carrier 1) and B (seated in carrier 2), on R29
    PINS = [360.0*k/p["N"] for k in range(p["N"])]
    RO = p["RO"]; ym, zm = p["ym"], p["zm"]
    AIR = pt(51.0, 187.5)                          # hole from the piston groove's floor, between two ring pins
    GPOST = [(47.0, -10.0), (62.0, -9.0)]          # the belt guard's two screws, inside the belt loop

    # ---------------------------------------------------------------- rotating parts (front end as iteration 6)
    fp = d.part("Faceplate (steel)", STEEL, COL["face"], stock="1018 cold-rolled flat 3/8 x 4 in (9.53 x 101.6), 105 long")
    fp.add("x", x["fp_r"], 0, C(0, 0, p["fp_r"]))
    fp.cut("x", x["fp_r"], 0, circs(42, FPA, 3.3), [C(sy*p["vise_m6"], sz*p["vise_m6"], 3.3) for sy in (1, -1) for sz in (1, -1)], C(0, 0, 5.5))
    fp.cut("x", -6.5, 0, circs(42, FPA, 5.5))
    fp.cut("x", x["fp_r"], x["fp_r"] + 6.5, [C(sy*p["vise_m6"], sz*p["vise_m6"], 5.5) for sy in (1, -1) for sz in (1, -1)])
    fp.cut("x", x["fp_r"], 0, R(48.6, -20.0, 55.0, 20.0))
    fp.done()

    nose = d.part("Spindle nose (steel)", STEEL, COL["steel"], stock="1018 cold-rolled flat 1/2 x 4 in (12.7 x 101.6), 100 long")
    nose.add("x", x["nose_r"], x["fp_r"], C(0, 0, p["nose_r"]))
    nose.add("x", x["ru_f"], x["nose_r"], C(0, 0, 29.5))
    nose.cut("x", x["ru_f"], x["fp_r"], circs(9, M6A, 3.3), circs(42, FPA, 3.3))
    nose.cut("x", x["fp_r"] - 6.5, x["fp_r"], circs(9, M6A, 5.5))
    for a in FPA:
        y0, z0 = pt(41.0, a); y1, z1 = pt(55.0, a)
        nose.cut("x", x["nose_r"], x["nose_r"] + 5.6, slot(y0, z0, y1, z1, 5.1))
    nose.done()

    hub = d.part("Hub disc A (steel)", STEEL, COL["steel"], stock="1018 cold-rolled flat 1/2 x 2-1/2 in (12.7 x 63.5), 65 long")
    hub.add("x", x["hubA_r"], x["ru_r"], C(0, 0, p["hub_r"]))
    hub.cut("x", x["hubA_r"], x["ru_r"], circs(21, M6H, 3.3), circs(9, M6A, 3.3))
    hub.cut("x", x["ru_r"] - 6.5, x["ru_r"], circs(21, M6H, 5.5))                          # M6 heads, front
    for a in M6A:
        y0, z0 = pt(9, a - 6.0); y1, z1 = pt(9, a + 6.0)
        hub.cut("x", x["hubA_r"], x["hubA_r"] + 7.0, slot(y0, z0, y1, z1, 5.1))
    hub.done()

    disc = d.part("Brake disc", ALU, COL["rotor"], stock="1/8 in (3.18) 6061 sheet, 110 x 110")
    disc.add("x", x["fl_r"], x["fl_f"], C(0, 0, p["fl_r"]))
    disc.cut("x", x["fl_r"], x["fl_f"], circs(21, M6H, 3.3), circs(9, M6A, 3.5))
    disc.cut("x", x["fl_r"], x["fl_f"] - p["web_t"], C(0, 0, p["fr_ri"] - 1.0), C(0, 0, p["web_ri"]))
    disc.done()

    hs = d.part("Hub spacer (steel)", STEEL, COL["steel"], stock="1018 cold-rolled flat 3/4 x 2-1/2 in (19.05 x 63.5), 65 long; rim polished for the seal")
    hs.add("x", x["hubB_r"], x["fl_r"], C(0, 0, p["hub_r"]))
    hs.cut("x", x["hubB_r"], x["fl_r"], circs(21, M6H, 3.3))
    hs.done()

    car1 = d.part("Carrier 1 (steel)", STEEL, COLC["car"], stock="1018 cold-rolled flat 1/2 x 3-1/2 in (12.7 x 88.9), 85 long")
    car1.add("x", x["car1_r"], x["car1_f"], C(0, 0, p["car_r"]))
    car1.cut("x", x["car1_f"] - 1.0, x["car1_f"], C(0, 0, p["car_r"] + 1), C(0, 0, p["hub_r"] - 0.6))     # front face stepped back 1 mm outside the hub: clears the seal
    SPR = bool(p.get("sprung")); THRU = SA if SPR else SA + SB
    car1.cut("x", x["car1_r"], x["car1_f"], circs(21, M6H, 3.3), circs(RO, THRU, 3.3))
    if SPR: car1.cut("x", x["car1_r"], x["car1_r"] + 7.5, circs(RO, SB, 7.5))                             # room for the nuts on the front ends of the sprung B sleeves
    car1.cut("x", x["car1_r"], x["car1_r"] + 8.0, C(0, 0, 14.0))                                           # front shaft bearing 6001
    car1.cut("x", x["car1_r"], x["slA_f"], circs(RO, SA, p["rq"]))                                         # seats of the three A sleeves
    for a in M6H:                                                                                          # hub screw nuts, rear face
        y0, z0 = pt(21, a - (3.0 if SPR else 5.0)); y1, z1 = pt(21, a + (3.0 if SPR else 5.0))
        car1.cut("x", x["car1_r"], x["car1_r"] + 5.6, slot(y0, z0, y1, z1, 5.1))
    for a in THRU:                                                                                      # sleeve screw nuts, front face, open to the rim
        y0, z0 = pt(RO, a); y1, z1 = pt(p["car_r"] + 6, a)
        car1.cut("x", x["car1_f"] - 5.6, x["car1_f"], slot(y0, z0, y1, z1, 5.1))
    car1.done()

    if SPR:
        import spider
        body = spider.outline(RO=RO, t=p["blade_t"], rim=p["car_r"])
        p["blade_k_n_per_mm"] = [float(v) for v in spider.blade_k(body, RO=RO, b_axial=p["car2_t"], z_off=(x["car2_f"] + x["car2_r"])/2 - (x["dB_f"] + x["dB_r"])/2)]
        car2 = d.part("Carrier 2 spider (steel)", STEEL, COLC["car"], stock="4140 pre-hardened or O1 ground flat stock 3/8 x 3-1/2 in (9.53 x 88.9), 85 long")
        car2.add("x", x["car2_r"], x["car2_f"], poly(list(body.exterior.coords)[:-1]))
        car2.cut("x", x["car2_r"], x["car2_f"], circs(RO, SA + SB, 3.3), C(0, 0, 8.6), circs(p["span_r"], SA, 3.25))   # three spanner holes, on the stiff arms
    else:
        car2 = d.part("Carrier 2 (steel)", STEEL, COLC["car"], stock="1018 cold-rolled flat 3/8 x 3-1/2 in (9.53 x 88.9), 85 long")
        car2.add("x", x["car2_r"], x["car2_f"], C(0, 0, p["car_r"]))
        car2.cut("x", x["car2_r"], x["car2_f"], circs(RO, SA + SB, 3.3), C(0, 0, 8.6), circs(21, [90, 270], 3.25))   # two spanner holes on R21
    car2.cut("x", x["slB_r"], x["car2_f"], circs(RO, SB, p["rq"]))                                         # seats of the three B sleeves
    car2.done()

    def cyc_disc(name, flip, xr, xf, col):
        q = d.part(name, STEEL, col, stock="O1 ground flat stock 1/4 x 4 in (6.35 x 101.6), both discs cut stacked from one 190 mm length")
        q.add("x", xr, xf, poly(disc_points(p, flip)))
        cy = -e if flip else e
        work = SB if flip else SA; over = SA if flip else SB
        q.cut("x", xr, xf, C(cy, 0, p["brg_r"]), [C(cy + RO*math.cos(math.radians(a)), RO*math.sin(math.radians(a)), p["rq"] + e + p["clear"]) for a in work],
              [C(cy + RO*math.cos(math.radians(a)), RO*math.sin(math.radians(a)), p["r_over"]) for a in over])
        q.done(); return q
    cyc_disc("Cycloid disc A", False, x["dA_r"], x["dA_f"], COLC["disc"])
    cyc_disc("Cycloid disc B", True, x["dB_r"], x["dB_f"], COLC["disc2"])

    for nm, cy, xr, xf in (("Eccentric bush A", e, x["bushA_r"], x["bush_f"]), ("Eccentric bush B", -e, x["bushB_r"], x["bushA_r"])):
        q = d.part(nm, STEEL, COLC["bush"], stock="1018 cold-rolled flat 3/8 in, faced to 9.00; bore and journal in one setup")
        q.add("x", xr, xf, C(cy, 0, 10.0), C(0, 0, 6.0)); q.done()

    # ---------------------------------------------------------------- standing plates
    def ties_and_ups(part, xr, xf, ups_face=None, land=19.0):
        part.cut("x", xr, xf, [C(a, b, 4.5) for a, b in TIES6])
        if ups_face is not None:
            for sy in (1, -1): upbolt6(part, zb, ups_face, +1, sy*UPY6)
            if xf - ups_face > land + 0.5:
                part.cut("z", zb - 0.01, zb + 0.2, R(ups_face + land, -p["W2"] - 1, xf + 1, p["W2"] + 1))

    p1 = d.part("P1 bearing plate", ALU, COL["plate"], stock="1-1/4 in (31.75) 6061 plate, 155 x 115")
    p1.add("x", x["p1_r"], x["p1_f"], outline6(p))
    p1.cut("x", x["p1_r"], x["p1_f"], C(0, 0, 37.0))
    p1.cut("x", x["ru_r"], x["p1_f"], C(0, 0, p["ru_od"]/2 + 0.05))
    p1.cut("x", x["p1_r"], x["rec_f"], C(0, 0, p["rec_r"]))
    M8A = [45*i for i in range(8)]
    p1.cut("x", x["rec_f"], x["ru_r"], circs(p["ru_out_pcd"]/2, M8A, 3.25))
    for a in M8A:
        y0, z0 = pt(41.65, a - 1.2); y1, z1 = pt(41.65, a + 1.2)
        p1.cut("x", x["rec_f"], x["rec_f"] + 6.0, slot(y0, z0, y1, z1, 3.65))
    ties_and_ups(p1, x["p1_r"], x["p1_f"], x["p1_r"])
    p1.done()

    c1 = d.part("C1 piston and seal plate", ALU, COL["plate2"], stock="3/4 in (19.05) 6061 plate, 155 x 115")
    c1.add("x", x["c1_r"], x["c_f"], outline6(p, 0.3))
    c1.cut("x", x["c1_r"], x["c_f"], C(0, 0, p["hub_r"] + 1.0))
    c1.cut("x", x["gr_floor"], x["c_f"], C(0, 0, p["gr_ro"]), C(0, 0, p["gr_ri"]))
    c1.cut("x", x["c1_r"], x["gr_floor"], C(AIR[0], AIR[1], 3.25))                          # air into the groove, between two ring pins
    c1.cut("x", x["c1_r"], x["seal_f"], C(0, 0, 37.5))                                      # lip seal 60 x 75 x 8
    c1.cut("x", x["c1_r"], x["c1_r"] + 0.6, C(0, 0, p["car_r"] + 1.0))                      # clears carrier 1's rim
    ties_and_ups(c1, x["c1_r"], x["c_f"])
    c1.done()

    def pin_plate(name, xr, xf, front):
        q = d.part(name, ALU if p["pins_g"] != STEEL else STEEL, COL["plate4"],
                   stock=("1/2 in (12.7) 6061 plate" if p["pins_g"] != STEEL else "1018 cold-rolled flat 1/2 x 6 in") + ", 155 x 115; both pin plates bored stacked")
        q.add("x", xr, xf, outline6(p, 0.3))
        q.cut("x", xr, xf, C(0, 0, p["car_r"] + 0.6), circs(p["Rp"], PINS, p["rp"]), C(PORT6[0], PORT6[1], 3.25))
        if front: q.cut("x", xf - 3.0, xf, slot(AIR[0], AIR[1], PORT6[0], PORT6[1], 3.25))  # air channel in the front face
        ties_and_ups(q, xr, xf); q.done(); return q
    pin_plate("G1 pin plate", x["g1_r"], x["g1_f"], True)
    mr = d.part("M ring spacer", ALU, COL["plate3"], stock="3/4 in (19.05) 6061 plate, 155 x 115, faced to %.2f" % (x["m_f"] - x["m_r"]))
    mr.add("x", x["m_r"], x["m_f"], outline6(p, 0.3))
    mr.cut("x", x["m_r"], x["m_f"], C(0, 0, 49.5), C(PORT6[0], PORT6[1], 3.25))
    ties_and_ups(mr, x["m_r"], x["m_f"]); mr.done()
    pin_plate("G2 pin plate", x["g2_r"], x["g2_f"], False)

    # D: rear plate with the motor ear toward +y
    w, b_, r_, s_ = p["W2"], zb, p["r_top"], p["shoulder"]
    dd = math.hypot(w, s_); aa = math.atan2(s_, w) + math.acos(r_/dd); ty, tz = r_*math.cos(aa), r_*math.sin(aa)
    L = lambda a_, c_: {"type": "line", "from": [round(a_[0], 4), round(a_[1], 4)], "to": [round(c_[0], 4), round(c_[1], 4)]}
    ey = 142.0; et = 4.0
    FRAME = it >= 4                                  # c4: the motor sits in a pocket with walls on three sides, not on an open web (motor mode 175 Hz in c3)
    if FRAME:
        ey = 148.0; et = s_
        ux, uz = ty - w, tz - s_; un = math.hypot(ux, uz); ux, uz = ux/un, uz/un
        th_ = math.acos(max(-1, min(1, -ux)))                       # angle between the ear's top edge (heading -y) and the shoulder
        tl = 6.0*math.tan(th_/2); P2 = (w + ux*tl, s_ + uz*tl)
        d_out = [L((-w, b_), (ey - 6, b_)), {"type": "arc", "center": [ey - 6, b_ + 6], "start": [ey - 6, b_], "end": [ey, b_ + 6]},
                 L((ey, b_ + 6), (ey, et - 6)), {"type": "arc", "center": [ey - 6, et - 6], "start": [ey, et - 6], "end": [ey - 6, et]},
                 L((ey - 6, et), (w + tl, et)), {"type": "arc", "center": [round(w + tl, 4), et + 6], "start": [round(P2[0], 4), round(P2[1], 4)], "end": [round(w + tl, 4), et]},
                 L(P2, (ty, tz)),
                 {"type": "arc", "center": [0, 0], "start": [round(ty, 4), round(tz, 4)], "end": [round(-ty, 4), round(tz, 4)]},
                 L((-ty, tz), (-w, s_)), L((-w, s_), (-w, b_))]
    else: d_out = [L((-w, b_), (ey - 6, b_)), {"type": "arc", "center": [ey - 6, b_ + 6], "start": [ey - 6, b_], "end": [ey, b_ + 6]},
             L((ey, b_ + 6), (ey, et - 6)), {"type": "arc", "center": [ey - 6, et - 6], "start": [ey, et - 6], "end": [ey - 6, et]},
             L((ey - 6, et), (w + 6, et)), {"type": "arc", "center": [w + 6, et + 6], "start": [w, et + 6], "end": [w + 6, et]},
             L((w, s_), (ty, tz)),
             {"type": "arc", "center": [0, 0], "start": [round(ty, 4), round(tz, 4)], "end": [round(-ty, 4), round(tz, 4)]},
             L((-ty, tz), (-w, s_)), L((-w, s_), (-w, b_))]
    dp = d.part("D rear plate and motor ear", ALU, COL["plate3"], stock="5/8 in (15.88) 6061 plate, 230 x 115")
    dp.add("x", x["d_r"], x["d_f"], d_out)
    dp.cut("x", x["d_r"], x["d_f"], C(0, 0, 10.0), C(PORT6[0], PORT6[1], 4.5), [C(a, b, 3.3) for a, b in GPOST])
    if SPR:
        for a in SA:                                                                       # one radial window per arm: the spanner pin (R20) and the A screw (R29)
            y0, z0 = pt(p["span_r"], a); y1, z1 = pt(RO, a)
            dp.cut("x", x["d_r"], x["d_f"], slot(y0, z0, y1, z1, 4.0))
    else:
        dp.cut("x", x["d_r"], x["d_f"], circs(RO, SA + SB, 4.0))
        for a in (90, 270):                                                                # windows for the setting spanner
            y0, z0 = pt(21, a - 12); y1, z1 = pt(21, a + 12)
            dp.cut("x", x["d_r"], x["d_f"], slot(y0, z0, y1, z1, 3.6))
    dp.cut("x", x["d_rec"], x["d_f"], C(0, 0, p["car_r"] + 1.0))                           # over carrier 2 and its screw heads
    dp.cut("x", x["d_r"], x["brg_f"], C(0, 0, 14.0))                                       # rear shaft bearing 6001
    for a, b in GPOST: dp.cut("x", x["d_f"] - 6.6, x["d_f"], slot(a, b - 1.0, a, b + 1.0, 5.1))
    if FRAME: dp.cut("x", x["web_f"], x["d_f"], rrect(w + 0.3, zb - 14, ym + 33.55, zm + 30.6, 3.5))   # motor pocket: walls toward the housing, above and outside
    else: dp.cut("x", x["web_f"], x["d_f"], R(w + 0.8, zb - 1, ey + 1, et + 7))            # the ear is a 6.56 mm web; the motor's flange sits on its front
    dp.cut("x", x["d_r"], x["web_f"], slot(ym - 3, zm, ym + 3, zm, 19.6),
           [slot(ym + sy*23.57 - 3, zm + sz*23.57, ym + sy*23.57 + 3, zm + sz*23.57, 3.25) for sy in (1, -1) for sz in (1, -1)])
    ties_and_ups(dp, x["d_r"], x["d_f"], x["d_r"], land=99.0)
    dp.done()

    base = d.part("Base plate (steel)", STEEL, COL["base"], stock="1018 cold-rolled flat 5/16 x 6 in (7.94 x 152.4), 210 long")
    bx0, bx1 = -164.0, 44.0
    tb = [(21, 62.5), (21, -62.5), (-154, 62.5), (-154, -62.5)]
    ups = [(x["p1_r"] + 10, UPY6), (x["p1_r"] + 10, -UPY6), (x["d_r"] + 8, UPY6), (x["d_r"] + 8, -UPY6)]
    base.add("z", zt, zb, rrect(bx0, -p["W2"], bx1, p["W2"], 6))
    BRK = [(6.0, 68.0), (-12.0, 68.0)]
    base.cut("z", zt, zb, [C(a, b, 4.5) for a, b in tb], [C(a, b, 4.5) for a, b in ups], [C(a, b, 3.25) for a, b in BRK])
    for a, b in ups: base.cut("z", zt, zt + 4.6, C(a, b, 8.2))
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
    seal = d.part("Shaft seal 60 x 75 x 8", "ABS", COL["rubber"], kind="bought")
    seal.add("x", x["seal_f"] - 8.0, x["seal_f"], C(0, 0, 37.5), C(0, 0, 30.0)); seal.done()
    tube = d.part("Air tube stub 6 x 80", "Brass", "#c9a94f", kind="bought")
    tube.add("x", x["d_r"] - 18, x["g1_f"] - 3.5, C(PORT6[0], PORT6[1], 3.0), C(PORT6[0], PORT6[1], 2.0)); tube.done()
    spl = d.part("Splash ring", "ASA", COL["printed"], kind="printed")
    spl.add("x", x["p1_f"], x["p1_f"] + 10.0, C(0, 0, 54.5), C(0, 0, 49.0)); spl.cut("x", x["p1_f"], x["p1_f"] + 10.0, R(-3.0, -55.5, 3.0, -48.0)); spl.done()

    ru = d.part("RU66 crossed roller bearing", STEEL, "#9aa3ad", kind="bought")
    ru.add("x", x["ru_r"], x["ru_f"], C(0, 0, 47.5), C(0, 0, 17.5))
    ru.cut("x", x["ru_r"], x["ru_f"], circs(41.5, M8A, 2.25), circs(22.5, [22.5 + 45*i for i in range(8)], 1.65))
    ru.cut("x", x["ru_f"] - 4.4, x["ru_f"], circs(41.5, M8A, 4.0)); ru.done()

    # ---------------------------------------------------------------- reducer: bought parts
    pins = d.part("Ring pin 8 m6 x 40 (ISO 8734)", STEEL, COLC["pin"], kind="bought")
    px0 = (x["g1_f"] + x["g2_r"])/2 + p["pin_l"]/2
    pins.add("x", px0 - p["pin_l"], px0, circs(p["Rp"], PINS, p["rp"])); pins.done(); pins.n_bodies = p["N"]
    for nm, angs, xr, xf in (("Output sleeves A (2 x IR 7x10x10.5 each)", SA, x["car2_f"], x["slA_f"]), ("Output sleeves B (2 x IR 7x10x10.5 each)", SB, x["slB_r"], x["slB_f"])):
        q = d.part(nm, STEEL, COLC["pin"], kind="bought")
        q.add("x", xr, xf, circs(RO, angs, p["rq"])); q.cut("x", xr, xf, circs(RO, angs, 3.5)); q.done(); q.n_bodies = 3
    for nm, cy, xr, xf in (("Bearing 6904-2RS (disc A)", e, x["bushA_r"], x["bush_f"]), ("Bearing 6904-2RS (disc B)", -e, x["bushB_r"], x["bushA_r"])):
        q = d.part(nm, STEEL, COLC["brg"], kind="bought"); q.add("x", xr, xf, C(cy, 0, p["brg_r"]), C(cy, 0, 10.0)); q.done()
    q = d.part("Bearing 6001-2RS (front)", STEEL, COLC["brg"], kind="bought"); q.add("x", x["car1_r"], x["car1_r"] + 8.0, C(0, 0, 14.0), C(0, 0, 6.0)); q.done()
    q = d.part("Bearing 6001-2RS (rear)", STEEL, COLC["brg"], kind="bought"); q.add("x", x["d_r"], x["brg_f"], C(0, 0, 14.0), C(0, 0, 6.0)); q.done()
    q = d.part("Shim ring 12 x 18 x 0.5", STEEL, "#d8d8a0", kind="bought"); q.add("x", x["bush_f"], x["car1_r"], C(0, 0, 9.0), C(0, 0, 6.0)); q.done()
    q = d.part("Spacer tube 12 x 16 x %.1f" % (x["bushB_r"] - x["brg_f"]), STEEL, COLC["shaft"], kind="bought", stock="cut from 16 x 2 steel tube and faced")
    q.add("x", x["brg_f"], x["bushB_r"], C(0, 0, 8.0), C(0, 0, 6.0)); q.done()
    sx_f = x["car1_r"] + 7.85; sx_r = sx_f - 80.0
    q = d.part("Input shaft 12 h6 x 80", STEEL, COLC["shaft"], kind="bought", stock="hardened precision shaft, cut to length by the seller")
    q.add("x", sx_r, sx_f, C(0, 0, 6.0)); q.done()
    r2 = p["z2"]*3/math.pi/2; r1 = p["z1"]*3/math.pi/2
    q = d.part("Pulley GT3 3 mm 60 teeth, 12 bore", ALU, COLC["pulley"], kind="bought")
    q.add("x", x["belt_r"] - 1.0, x["belt_f"] + 1.0, C(0, 0, r2 - 0.4), C(0, 0, 6.0)); q.add("x", x["belt_f"] + 1.0, x["d_r"] - 1.0, C(0, 0, 15.0), C(0, 0, 6.0)); q.done()
    q = d.part("Pulley GT3 3 mm 20 teeth, 8 bore", ALU, COLC["pulley"], kind="bought")
    q.add("x", x["belt_r"] - 1.0, x["belt_f"] + 1.0, C(ym, zm, r1 - 0.4), C(ym, zm, 4.0)); q.add("x", x["belt_f"] + 1.0, x["d_r"] - 1.5, C(ym, zm, 8.0), C(ym, zm, 4.0)); q.done()
    q = d.part("Belt " + p["belt"], "ABS", COLC["belt"], kind="bought")
    q.add("x", x["belt_r"], x["belt_f"], poly(hull((0, 0), r2 + 0.6, (ym, zm), r1 + 0.6))); q.cut("x", x["belt_r"], x["belt_f"], poly(hull((0, 0), r2 - 0.35, (ym, zm), r1 - 0.35))); q.done()
    q = d.part("Motor rear cover and gland (part of 23E1KIP65-20)", ALU, "#2a2a2a", kind="bought")
    q.add("x", x["web_f"] + 85.8, x["motor_end"], rrect(ym - 28, zm - 28, ym + 28, zm + 28, 4)); q.done()

    if it >= 3 and SPR:                                                                    # c3: the three windows in D are closed with printed plugs (grease in, chips out)
        plug = d.part("Window cover", "ASA", COL["printed"], kind="printed")
        ends = [(pt(p["span_r"], a), pt(RO, a)) for a in SA]
        plug.add("x", x["d_r"] - 0.8, x["d_r"], C(0, 0, 33.5), C(0, 0, 16.0))                 # a thin ring behind D, under the pulley
        plug.add("x", x["d_r"], x["d_r"] + 5.0, [slot(u[0], u[1], v[0], v[1], 3.95) for u, v in ends])   # three plugs that snap into the windows
        plug.done()

    guard = d.part("Belt guard", "ASA", COL["printed"], kind="printed")
    gout = hull((0, 0), r2 + 8.0, (ym, zm), r1 + 8.0); gin = hull((0, 0), r2 + 5.6, (ym, zm), r1 + 5.6)
    guard.add("x", x["guard_r"], x["d_r"], poly(gout))
    guard.cut("x", x["guard_r"] + 2.4, x["d_r"], poly(gin))
    guard.add("x", x["guard_r"], x["d_r"], [C(a, b, 6.5) for a, b in GPOST])
    guard.cut("x", x["guard_r"], x["d_r"], [C(a, b, 3.3) for a, b in GPOST])
    guard.cut("x", x["d_r"] - 10.0, x["d_r"], C(TIES6[0][0], TIES6[0][1], 9.5))            # notch over the tie rod's nut
    guard.done()

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

    # ---------------------------------------------------------------- tailstock (as iteration 6)
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

    vs = d.part("Vise XinDian H54-8-55", STEEL, COL["vise"], kind="bought")
    hl, hw_ = p["vise_l"]/2, p["vise_w"]/2
    vs.add("x", 0, 33, rrect(-hl, -hw_, hl, hw_, 2)); vs.add("x", 33, 44, R(20, -hw_, 39, hw_), R(-39, -hw_, -20, hw_))
    vs.cut("x", 0, 10, [C(sy*14.14, sz*14.14, 2.5) for sy in (1, -1) for sz in (1, -1)], C(0, 0, 4.25),
           [C(sy*20.5, sz*20.5, 2.1) for sy in (1, -1) for sz in (1, -1)], C(18.5, 0, 4.5), C(-18.5, 0, 4.5), C(0, 18.5, 4.5), C(0, -18.5, 4.5))
    vs.cut("x", 14, 30, R(-hl - 1, -8, -hl + 14, 8), R(hl - 14, -8, hl + 1, 8)); vs.done()

    s_mot = d.vendor("23E1KIP65-20", STEP_M23)

    # ---------------------------------------------------------------- assembly
    def station(nm):
        for key, st in (("Faceplate", "face"), ("Spindle nose", "nose"), ("Base plate", "base"), ("P1", "p1"), ("C1", "c"), ("G1", "g1"), ("M ring", "m"), ("G2", "g2"), ("D ", "d"),
                        ("Hub spacer", "hubb"), ("Hub disc", "huba"), ("Brake disc", "rotor"), ("Brake piston", "brake"), ("Friction", "p1"), ("O-ring", "brake"), ("Shaft seal", "c"),
                        ("Air tube", "d"), ("Tailstock base", "tailbase"), ("Tail", "tail"), ("Vise", "vise"), ("Splash", "splash"),
                        ("Sensor", "base"), ("Inductive", "base"), ("Heat", "base"), ("RU66", "ru"), ("Carrier 1", "car1"), ("Carrier 2", "car2"), ("Cycloid disc A", "discA"),
                        ("Cycloid disc B", "discB"), ("Eccentric bush A", "discA"), ("Eccentric bush B", "discB"), ("Bearing 6904-2RS (disc A)", "discA"), ("Bearing 6904-2RS (disc B)", "discB"),
                        ("Ring pin", "pins"), ("Output sleeves", "sleeves"), ("Bearing 6001-2RS (front)", "car1"), ("Bearing 6001-2RS (rear)", "d"), ("Shim", "discA"), ("Spacer tube", "shaft"),
                        ("Input shaft", "shaft"), ("Window cover", "belt"), ("Pulley", "belt"), ("Belt guard", "guard"), ("Belt", "belt"), ("Motor", "motor")):
            if nm.startswith(key): return st
        return "p1"
    for part in d.parts:
        part.inst = d.place(part, group=station(part.name))
        for k in range(1, getattr(part, "n_bodies", 1)): d.place_body(part.studio, [part.body[0], k], part.name, group=station(part.name))
    d.place_body(s_mot, [1, 2], "Motor 23E1KIP65-20", position=(x["web_f"], ym, zm), rotate=axis_angle(ry(-90)), group="motor")

    g = {"v": "p1"}
    def scr(size, L_, seat, dirn, std="ISO 4762"): return d.screw(size, L_, seat, dirn, std=std, group=g["v"])
    def nut(size, base_, dirn): return d.std("ISO 4032", size, base_, dirn, group=g["v"])
    def washer(size, base_, dirn): return d.std("ISO 7089", size, base_, dirn, group=g["v"])
    for a in FPA:
        y, z = pt(42, a)
        g["v"] = "face"; scr("M6", 14, (-6.5, y, z), (-1, 0, 0)); g["v"] = "nose"; nut("M6", (x["nose_r"] + 5.6, y, z), (-1, 0, 0))
    g["v"] = "face"
    for sy in (1, -1):
        for sz in (1, -1): scr("M6", 12, (x["fp_r"] + 6.5, sy*14.14, sz*14.14), (1, 0, 0))
    for a in M6A:
        y, z = pt(9, a)
        g["v"] = "nose"; scr("M6", 35, (x["fp_r"] - 6.5, y, z), (-1, 0, 0)); g["v"] = "huba"; nut("M6", (x["hubA_r"] + 7.0, y, z), (-1, 0, 0))
    for a in M6H:                                                                       # hub A + brake disc + hub spacer + carrier 1: 6 x M6 x 40
        y, z = pt(21, a); g["v"] = "huba"; scr("M6", 40, (x["ru_r"] - 6.5, y, z), (-1, 0, 0)); g["v"] = "car1"; nut("M6", (x["car1_r"] + 5.6, y, z), (-1, 0, 0))
    for a in M8A:
        y, z = pt(41.5, a); g["v"] = "ru"; scr("M4", 20, (x["ru_f"] - p["ru_cb"], y, z), (-1, 0, 0))
        g["v"] = "p1"; nut("M4", (x["rec_f"] + 6.0, y, z), (-1, 0, 0))
    for a in THRU:                                                                      # carrier 2 + sleeve + carrier 1: M6 x 40 from the rear
        y, z = pt(RO, a); g["v"] = "car2"; scr("M6", 40, (x["car2_r"], y, z), (1, 0, 0)); g["v"] = "car1"; nut("M6", (x["car1_f"] - 5.6, y, z), (1, 0, 0))
    if SPR:
        for a in SB:                                                                    # sprung B sleeves: M6 x 35 through the blade's pad and the sleeve, nut on the sleeve's front end
            y, z = pt(RO, a); g["v"] = "car2"; scr("M6", 35, (x["car2_r"], y, z), (1, 0, 0)); nut("M6", (x["slB_f"], y, z), (1, 0, 0))
    for (y, z) in TIES6:
        g["v"] = "ru"; washer("M8", (x["p1_f"], y, z), (1, 0, 0)); scr("M8", 120, (x["p1_f"] + 1.6, y, z), (-1, 0, 0))
        g["v"] = "motor"; washer("M8", (x["d_r"], y, z), (-1, 0, 0)); nut("M8", (x["d_r"] - 1.6, y, z), (-1, 0, 0))
    for sy in (1, -1):
        for sz in (1, -1):                                                              # motor: 4 x M5 x 20 from the front, washers and nuts behind the web
            g["v"] = "motor"; scr("M5", 20, (x["web_f"] + 5.0, ym + sy*23.57, zm + sz*23.57), (-1, 0, 0))
            washer("M5", (x["d_r"], ym + sy*23.57, zm + sz*23.57), (-1, 0, 0)); nut("M5", (x["d_r"] - 1.0, ym + sy*23.57, zm + sz*23.57), (-1, 0, 0))
    for a, b in GPOST:
        g["v"] = "guard"; washer("M6", (x["guard_r"], a, b), (-1, 0, 0)); scr("M6", 45, (x["guard_r"] - 1.6, a, b), (1, 0, 0)); g["v"] = "d"; nut("M6", (x["d_f"] - 6.6, a, b), (1, 0, 0))
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
    fit = [[x["motor_r"], -85, -H], [200.0, 150, 60]]
    for view, nm in (("iso_front_right", "iso"), ("iso_back_left", "iso-back"), ("iso_back_right", "iso-back-right"), ("front", "front"), ("top", "top"), ("right", "right"), ("back", "back")):
        extra.append({"tool": "render_assembly", "args": {"assembly": 0, "view": view, "width": 1800, "height": 1200, "fit_box": fit, "path": f"render-{nm}.png"}})
    for part in d.parts:
        stem = part.name.lower().replace(" ", "-").replace("/", "-").replace("(", "").replace(")", "").replace(",", "").replace(".", "_")
        part.stem = stem
        extra += [{"tool": "switch_part_studio", "args": {"index": part.studio}},
                  {"tool": "export", "args": {"format": "step", "path": f"parts/{stem}.step"}},
                  {"tool": "render", "args": {"view": "iso", "width": 900, "height": 700, "path": f"parts/{stem}.png"}},
                  {"tool": "mass_properties", "args": {}}]
    res = run(d.script(extra), outdir, "build")
    meta = dict(params=p, x=x, parts=[dict(name=q.name, stem=q.stem, studio=q.studio, body=q.body, material=q.material, kind=q.kind, stock=q.stock, inst=q.inst,
                                           n=getattr(q, "n_bodies", 1)) for q in d.parts], instances=d.inst)
    json.dump(meta, open(os.path.join(outdir, "meta.json"), "w"), indent=1)
    return d, res, meta


if __name__ == "__main__":
    axlib.RCAD = os.path.expanduser("~/src/cad-rust-app-4axis/target/release/rcad")
    it = int(sys.argv[1]); out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(ROOT, "cycloidal", f"c{it}"); os.makedirs(os.path.join(out, "parts"), exist_ok=True)
    d, res, meta = buildc(it, out)
    bad = failures(res)
    print("failures:", len(bad))
    for b in bad[:30]: print("  ", b)
    names = {i: n for i, n, _ in meta["instances"]}
    ai = [q["result"] for q in res if q.get("tool") == "assembly_info" and "result" in q]
    if ai:
        json.dump(ai[0], open(os.path.join(out, "assembly_info.json"), "w"), indent=1)
        hits = ai[0].get("interference", [])
        print("instances", len(ai[0].get("instances", [])), "interferences", len(hits))
        for h in sorted(hits, key=lambda h: -h["volume_mm3"])[:60]:
            print(f"   {names.get(h['a'], h['a'])[:42]:42s} x {names.get(h['b'], h['b'])[:42]:42s} {h['volume_mm3']:.2f} mm3")
