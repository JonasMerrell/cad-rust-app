#!/usr/bin/env python3
"""Hand calculations for one iteration of the A axis, on top of its FEA results.

    calc.py <iteration>      -> iter<N>/calc.json and iter<N>/calc.md

Every number carries a tag:  D = vendor datasheet,  C = calculated here,  F = rCAD finite elements,
G = guess / typical value (the report lists these as assumptions).
"""
import sys, os, json, math
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import phys
from build import params, stack, ROOT

# ------------------------------------------------------------------ fixed inputs
E_AL, E_ST, G_AL = 68.9e9, 200e9, 26e9
R_MAX = 0.0462            # m: half the diagonal of the 75 x 54 vise = the largest radius anything swings      (C)
R_TYP = 0.025             # m: a 50 mm part
DYN = 2.0                 # peak / mean force for interrupted cutting (see REPORT, loads)                       (G, argued)
GEAR = dict(name="HSS-25-100-I-D14", ratio=100, k1=3.1e4, k2=5.0e4, k3=5.7e4, t1=14.0, t2=48.0,              # D (Harmonic Drive CSF/CSG-25 table,
            rated=67.0, peak=157.0, momentary=284.0, backlash_arcsec=20.0, hysteresis_arcsec=60.0,            #    identical ratings to the Laifual unit)
            accuracy_arcsec=60.0, bearing_km=24.2e4, bearing_moment_allow=156.0, j_in=0.413e-4,
            eff=0.70, no_load_start_nm=0.085, max_in_rpm=5600, avg_in_rpm=3500)                                # eff, starting torque: G (HD graphs)
MOTOR = dict(name="34E1KIP65-45", hold=4.5, j_rotor=1.8e-4, counts=4000, step_deg=1.8, mass=2.1,               # D (datasheet: 1800 g.cm2; 2.1 kg reseller page)
             torque_at=dict([(100, 4.2), (300, 3.96), (600, 3.24), (900, 1.95), (1200, 0.61)]))                # D: vendor pull-out curve at 60 V, 6 A (read off the plot)
RU85 = dict(dp=85.0, dw=6.0, lw=4.8, z=36, preload_um=2.0, c_dyn=20.3e3, c0=29.5e3, knock=0.5)                  # C, C0, dp: D (THK); rollers G (fit C0); knock G
KC = 2000.0               # N/mm^3: normal stiffness of a bolted, milled interface per unit area               (G: 500..5000 published)


def tilt_k_annulus(ro, ri, kc=KC): return kc*math.pi*(ro**4 - ri**4)/4*1e-3          # N·m/rad (mm in)
def tilt_k_rect(b, h, kc=KC): return kc*b*h**3/12*1e-3                                # about the axis along b


def main(it):
    out = os.path.join(ROOT, f"iter{it}")
    p = params(it); x = stack(p)
    fea = json.load(open(os.path.join(out, "fea", "results.json"))) if os.path.exists(os.path.join(out, "fea", "results.json")) else {}
    R = {"iteration": it}
    md = [f"# Iteration {it}: calculated results\n", "Tags: D datasheet, C calculated, F finite elements (rCAD), G guess.\n"]

    # ---------------------------------------------------------------- loads
    rough = phys.cutting(850, 9000); never = phys.cutting(950, 9000); fin = phys.cutting(450, 24000)
    pk = phys.kienzle_peak_ratio(1.0, 6.0, 3000/9000/3)
    L = dict(rough_side=rough["side"], rough_peak=rough["side"]*DYN, never_side=never["side"], fin_side=fin["side"], fin_peak=fin["side"]*DYN,
             rough_ft=rough["ft"], fin_ft=fin["ft"], rough_axial=rough["fa"], fin_axial=fin["fa"], tooth_hz_9k=450.0, tooth_hz_24k=1200.0,
             straight_tooth_peak_over_mean=pk["ratio"])
    R["loads"] = L
    md += ["## Loads\n", "| Case | Tangential N | Sideways mean N | Peak N (x2) | Axial N | Torque at 46 mm N·m (mean / peak) |", "|---|---|---|---|---|---|",
           f"| Roughing 850 W, 9000 rpm | {rough['ft']:.0f} | {rough['side']:.0f} | {rough['side']*DYN:.0f} | {rough['fa']:.0f} | {rough['side']*R_MAX:.1f} / {rough['side']*DYN*R_MAX:.1f} |",
           f"| Never-exceed 950 W, 9000 rpm | {never['ft']:.0f} | {never['side']:.0f} | {never['side']*DYN:.0f} | {never['fa']:.0f} | {never['side']*R_MAX:.1f} / {never['side']*DYN*R_MAX:.1f} |",
           f"| Finishing 450 W, 24 000 rpm | {fin['ft']:.0f} | {fin['side']:.0f} | {fin['side']*DYN:.0f} | {fin['fa']:.0f} | {fin['side']*R_MAX:.1f} / {fin['side']*DYN*R_MAX:.1f} |", ""]

    # ---------------------------------------------------------------- bearing
    if it == 1:
        k_tilt = GEAR["bearing_km"]                       # D
        # radial stiffness is not published: scale a roller model of the unit's bearing to the published tilt stiffness
        b = phys.CrossedRoller(64.0, 4.0, 3.2, 40, 2.0).summary()
        scale = k_tilt/b["k_tilt_Nm_per_rad"]
        k_rad = b["k_radial_N_per_um"]*1e6*scale; k_ax = b["k_axial_N_per_um"]*1e6*scale
        x_brg = x["hss_out"] - 9.0                        # centre of the unit's crossed-roller bearing
        brg = dict(kind="gear unit's own crossed-roller bearing", k_tilt=k_tilt, k_rad=k_rad, k_ax=k_ax, tag="tilt D, radial C (scaled model)")
    else:
        cr = phys.CrossedRoller(RU85["dp"], RU85["dw"], RU85["lw"], RU85["z"], RU85["preload_um"]); s = cr.summary()
        k_tilt = s["k_tilt_Nm_per_rad"]*RU85["knock"]; k_rad = s["k_radial_N_per_um"]*1e6*RU85["knock"]; k_ax = s["k_axial_N_per_um"]*1e6*RU85["knock"]
        x_brg = (x["ru_f"] + x["ru_r"])/2
        brg = dict(kind="THK RU85 crossed roller", k_tilt=k_tilt, k_rad=k_rad, k_ax=k_ax, theory=s, knock=RU85["knock"],
                   tag="C: Palmgren line contact, 36 rollers of 6 x 4.8 mm (G, fits C0), 2 um preload (G), x0.5 for ring and seat flexing (G)")
    R["bearing"] = brg

    # ---------------------------------------------------------------- bolted interfaces (tilt)
    joints = []
    if it == 1:
        joints.append(("nose / gear output flange (r 10..30)", tilt_k_annulus(30, 10.5)))
    else:
        joints.append(("nose / bearing inner ring (r 28..41.5)", tilt_k_annulus(41.5, 28)))
    joints.append(("faceplate / nose (r 0..55)", tilt_k_annulus(55, 0)))
    joints.append(("vise / faceplate (75 x 54, weak axis)", tilt_k_rect(75, 54)))
    if it == 1:
        joints.append(("plate / base (19 x 140 strip)", tilt_k_rect(140, 19.05)))
    else:
        a = 156*31.75; dd = (x["p1_f"] + x["p1_r"])/2 - (x["d_f"] + x["d_r"])/2
        joints.append(("plates / base (two 31.75 x 156 strips)", KC*(2*(a*(dd/2)**2 + 156*31.75**3/12))*1e-3))
    R["joints"] = [dict(name=n, k_tilt_nm_per_rad=k) for n, k in joints]

    # ---------------------------------------------------------------- tool-point deflection
    rows = []; defl = {}
    for tag in ("chuck", "overhang"):
        if "head" not in fea or tag not in fea["head"]: continue
        h = fea["head"][tag]; xe = h["x_load"]; bar = h["bar_len"]
        arm = (xe - x_brg)/1000.0
        for d_ in ("Fz", "Fy"):
            st = h["static"].get(d_, {})
            if "tool" not in st: continue
            f_struct = st["tool"]["magnitude_mm"]*1e-3/391.0                      # m/N, bonded structure incl. the steel test bar
            c_bar = (bar/1000 + 0.011)**3/(3*E_AL*phys.i_rect(0.04, 0.04))          # the bar's own bending from the jaw grip (the FE model is all aluminium)
            c_brg = 1/k_rad + arm**2/k_tilt
            c_j = 0.0; jl = []
            for n, k in joints:
                if "base" in n and d_ == "Fy":
                    k = k*(156.0/31.75)**2 if it > 1 else k*(140/19.05)**2          # sideways the base joint is as wide as the plate
                lever = (xe - (0.0 if "vise" in n else -12.7 if "faceplate" in n else x_brg))/1000.0 if "base" not in n else None
                if lever is None:
                    hgt = p["H"] - p["base_t"]                                    # axis above the base joint, mm
                    lv = math.hypot((xe - (x["p1_f"] + (x["d_r"] if it > 1 else x["p1_r"]))/2), 0)/1000.0 if d_ == "Fz" else hgt/1000.0
                    c = lv**2/k
                else:
                    c = lever**2/k
                c_j += c; jl.append((n, c))
            tot = f_struct + c_brg + c_j
            defl[(tag, d_)] = dict(struct=f_struct, bar=c_bar, bearing=c_brg, joints=c_j, total=tot, total_less_bar=tot - c_bar, joint_list=jl)
            rows.append((tag, d_, xe, f_struct, c_bar, c_brg, c_j, tot))
    R["tool_point"] = {f"{a}_{b}": {k: (v if k != "joint_list" else [(n, c) for n, c in v]) for k, v in d.items()} for (a, b), d in defl.items()}
    md += ["## Deflection at the tool point, headstock only (no tailstock)\n",
           "Load at the end of a 40 x 40 test bar gripped in the vise. 'Structure' is the bonded FE model (F; every part aluminium, so the steel vise is modelled too soft); the bearing (C) and the bolted joints (G: contact stiffness "
           f"{KC:.0f} N/mm3) are added in series. µm per the mean roughing force {L['rough_side']:.0f} N, and per the finishing force {L['fin_side']:.0f} N.\n",
           "| Load point | Dir | Structure F | of which the test bar C | Bearing C | Joints G | Total rough µm | Total less the bar, rough µm | Finishing µm (less the bar) |",
           "|---|---|---|---|---|---|---|---|---|"]
    for tag, d_, xe, fs, cb, cg, cj, tot in rows:
        f = L["rough_side"]*1e6; g = L["fin_side"]*1e6
        md.append(f"| {tag}, {xe - 44:.0f} mm past the jaws | {d_} | {fs*f:.1f} | {cb*f:.1f} | {cg*f:.1f} | {cj*f:.1f} | {tot*f:.1f} | {(tot - cb)*f:.1f} | {(tot - cb)*g:.2f} |")
    md.append("")
    if "steel" in fea and "tool" in fea["steel"] and ("overhang", "Fz") in defl:
        st_ = fea["steel"]["tool"]["magnitude_mm"]*1e-3/391.0; al_ = defl[("overhang", "Fz")]["struct"]
        c_bar_st = (0.1 + 0.011)**3/(3*E_ST*phys.i_rect(0.04, 0.04))
        R["steel_check"] = dict(struct_real_materials=st_, struct_all_aluminium=al_, bar_steel=c_bar_st)
        md += [f"Check with real materials (steel vise, bearing ring and test bar; direct solver, 6 mm mesh): the structure column at 100 mm, vertical, is "
               f"{st_*L['rough_side']*1e6:.1f} µm instead of {al_*L['rough_side']*1e6:.1f} µm, of which the steel bar itself is {c_bar_st*L['rough_side']*1e6:.1f} µm (F). "
               f"So the all-aluminium numbers above overstate the structure by about {(al_ - st_)*L['rough_side']*1e6:.0f} µm at that point.", ""]

    # ---------------------------------------------------------------- with the tailstock: a part between the vise and the centre
    if it >= 2 and "tail" in fea and ("overhang", "Fz") in defl:
        kt = {}
        for d_ in ("Fz", "Fy"):
            t = fea["tail"][d_]
            kt[d_] = 100.0/(t["tip"]["magnitude_mm"]*1e-3) if "tip" in t else 100.0/(t["max_disp_mm"]*1e-3)
        # centre in a 90 degree hole in aluminium: Hertz-like local contact, taken as 1e8 N/m (G)
        k_centre = 1.0e8
        res_t = {}
        for (dia, Lw) in ((0.025, 0.12), (0.040, 0.12), (0.016, 0.10)):
            EI = E_AL*phys.i_round(dia)
            # beam from the jaw face (x=0) to the centre (x=Lw): left end on a translational + rotational spring (the headstock at the chuck), right end on a spring
            c_h = defl[("chuck", "Fz")]["total_less_bar"]              # m/N at the chuck (translation)
            arm0 = 0.054 - x_brg/1000.0
            k_rot = 1.0/((defl[("overhang", "Fz")]["total_less_bar"] - defl[("chuck", "Fz")]["total_less_bar"])/((0.144 - x_brg/1000.0)**2 - arm0**2))
            k_h = 1.0/c_h; k_t = 1.0/(1.0/kt["Fz"] + 1.0/k_centre)
            def solve(with_tail, a):
                # Euler beam by finite elements (40 elements), unit load at a
                n = 40; le = Lw/n; nd = 2*(n + 1); K = np.zeros((nd, nd))
                ke = EI/le**3*np.array([[12, 6*le, -12, 6*le], [6*le, 4*le*le, -6*le, 2*le*le], [-12, -6*le, 12, -6*le], [6*le, 2*le*le, -6*le, 4*le*le]])
                for e in range(n): K[2*e:2*e + 4, 2*e:2*e + 4] += ke
                K[0, 0] += k_h; K[1, 1] += k_rot
                if with_tail: K[2*n, 2*n] += k_t
                f = np.zeros(nd); i = int(round(a/le)); f[2*i] = 1.0
                return np.linalg.solve(K, f)[2*i]
            mid = solve(True, Lw/2); free_mid = solve(False, Lw/2); free_end = solve(False, Lw)
            res_t[f"d{dia*1000:.0f}_L{Lw*1000:.0f}"] = dict(with_tail_mid=mid, no_tail_mid=free_mid, no_tail_end=free_end, k_tail=k_t, k_rot_head=k_rot)
        R["tailstock"] = dict(k_tail_fz=kt["Fz"], k_tail_fy=kt["Fy"], k_centre_contact=k_centre, parts=res_t)
        md += ["## With and without the tailstock (aluminium bar between the jaws and the centre)\n",
               f"Tailstock stiffness at the centre: {kt['Fz']/1e6:.0f} N/µm vertical, {kt['Fy']/1e6:.0f} N/µm sideways (F, bonded); centre-in-hole contact 100 N/µm (G).\n",
               "| Part | No tailstock, at the free end, rough µm | No tailstock, mid-length µm | With tailstock, mid-length, rough µm | With tailstock, finishing µm |", "|---|---|---|---|---|"]
        for k, v in res_t.items():
            f = L["rough_side"]*1e6; g = L["fin_side"]*1e6
            md.append(f"| Ø{k[1:].replace('_L', ' x ')} mm | {v['no_tail_end']*f:.0f} | {v['no_tail_mid']*f:.1f} | {v['with_tail_mid']*f:.1f} | {v['with_tail_mid']*g:.2f} |")
        md.append("")

    # ---------------------------------------------------------------- torsion
    def wind(T): return phys.harmonic_windup_rad(T, GEAR["k1"], GEAR["k2"], GEAR["k3"], GEAR["t1"], GEAR["t2"])
    tors = {}
    k_brake = None
    if it >= 3 and "brake" in fea and fea["brake"].get("rotation_rad"):
        k_disc = fea["brake"]["torque_nmm"]/1000.0/fea["brake"]["rotation_rad"]                   # F
        k_lin = 0.5e9*math.pi*(0.061**2 - (p["fr_ri"]/1000)**2)/0.00025*((0.061 + p["fr_ri"]/1000)/2)**2   # paper lining in shear, G = 0.5 GPa (G)
        k_brake = 1/(1/k_disc + 1/k_lin)
        tors["k_disc_fea"] = k_disc; tors["k_lining"] = k_lin; tors["k_brake"] = k_brake
    if it >= 4 and k_brake is None:
        # the thin-web disc would not solve (the remote-moment constraint overflows the direct solver on a mesh fine enough for a 1 mm web).
        # Hand formula for a flat ring in shear, K = 4 pi G t / (1/ri^2 - 1/ro^2), with the hub's share taken from iteration 3's FE result.
        f3 = os.path.join(ROOT, "iter3", "fea", "results.json")
        ring = lambda t, ri, ro: 4*math.pi*G_AL*t/(1/ri**2 - 1/ro**2)
        k_web = ring(p["web_t"]/1000, 0.038, p["fr_ri"]/1000)
        c_hub = 0.0
        if os.path.exists(f3):
            b3 = json.load(open(f3)).get("brake", {})
            if b3.get("rotation_rad"):
                k3 = b3["torque_nmm"]/1000.0/b3["rotation_rad"]; c_hub = max(0.0, 1/k3 - 1/ring(0.0025, 0.038, 0.047))
        k_disc = 1/(c_hub + 1/k_web)
        k_lin = 0.5e9*math.pi*(0.061**2 - (p["fr_ri"]/1000)**2)/0.00025*((0.061 + p["fr_ri"]/1000)/2)**2
        k_brake = 1/(1/k_disc + 1/k_lin)
        tors["k_disc_fea"] = k_disc; tors["k_lining"] = k_lin; tors["k_brake"] = k_brake; tors["k_disc_source"] = "C: ring formula, hub share from iteration 3's FE"
    cases = [("roughing mean", L["rough_side"]), ("roughing peak", L["rough_peak"]), ("finishing mean", L["fin_side"])]
    md += ["## Torsion: windup at the work surface\n",
           f"Gear: K1/K2/K3 = {GEAR['k1']:.0f}/{GEAR['k2']:.0f}/{GEAR['k3']:.0f} N·m/rad up to {GEAR['t1']}/{GEAR['t2']} N·m (D). "
           + (f"Brake: disc {tors['k_disc_fea']:.3g} N·m/rad ({tors.get('k_disc_source', 'F')}) in series with the lining {tors['k_lining']:.3g} (G) = {k_brake:.3g} N·m/rad.\n" if k_brake else "No brake in this iteration.\n"),
           "| Case | Radius mm | Torque N·m | Gear only: windup mrad | Gear only: µm at the surface | " + ("With brake: µm |" if k_brake else ""), "|---|---|---|---|---|" + ("---|" if k_brake else "")]
    tl = []
    for nm, F in cases:
        for r in (R_MAX, R_TYP):
            T = F*r; w = wind(T); um = w*r*1e6
            row = dict(case=nm, r_mm=r*1000, torque=T, windup_mrad=w*1000, um=um)
            if k_brake:
                kk = k_brake + GEAR["k1"]; row["um_brake"] = T/kk*r*1e6
            tl.append(row)
            md.append(f"| {nm} | {r*1000:.0f} | {T:.1f} | {w*1000:.3f} | {um:.1f} |" + (f" {row['um_brake']:.2f} |" if k_brake else ""))
    md.append("")
    tors["table"] = tl; R["torsion"] = tors

    # ---------------------------------------------------------------- brake
    if it >= 3:
        area = math.pi*((p["gr_ro"])**2 - (p["gr_ri"])**2)                                         # mm2
        press = 0.6                                                                               # MPa, shop air (G: not confirmed)
        f_seal = 2*60.0                                                                           # N lost to the two O-rings (G)
        N = area*press - f_seal; rm = (61.0 + 47.0)/2/1000
        br = dict(area_mm2=area, pressure_mpa=press, clamp_n=N, r_mean_m=rm, torque_mu03=0.3*N*rm, torque_mu06=0.6*N*rm,
                  need_peak=L["rough_peak"]*R_MAX, need_mean=L["rough_side"]*R_MAX,
                  disc_bend_stress_mpa=0.0)
        # the web between the hub (r 38) and the friction ring bends as a clamped-guided strip: force per width E t^3 d / L^3, stress 3 E t d / L^2
        Lw = (p["fr_ri"] - 38.0)/1000; tw = p["web_t"]/1000; rmid = (p["fr_ri"] + 38.0)/2/1000
        for nm_, gap in (("nominal", p["fl_gap"]/1000), ("worst", (p["fl_gap"] + 0.1)/1000)):
            br[f"web_force_{nm_}"] = E_AL*tw**3*gap/Lw**3*2*math.pi*rmid
            br[f"web_stress_{nm_}_mpa"] = 3*E_AL*tw*gap/Lw**2/1e6
        br["disc_bend_stress_mpa"] = br["web_stress_nominal_mpa"]
        rm = (61.0 + p["fr_ri"])/2/1000; br["r_mean_m"] = rm
        Nn = N - br["web_force_nominal"]; br["clamp_net_n"] = Nn
        br["torque_mu03"] = 0.3*Nn*rm; br["torque_mu06"] = 0.6*Nn*rm
        R["brake"] = br
        md += ["## Brake\n",
               f"- Piston ring Ø{2*p['gr_ri']:.0f} to Ø{2*p['gr_ro']:.1f}: {area:.0f} mm² (C). At {press} MPa (G: shop pressure not confirmed) less {f_seal:.0f} N seal drag (G): clamp force {N:.0f} N (C).",
               f"- Holding torque at mean radius {br['r_mean_m']*1000:.0f} mm: {br['torque_mu03']:.0f} N·m with µ 0.3, {br['torque_mu06']:.0f} N·m with µ 0.6 (C; µ of the bonded abrasive lining on aluminium is G, 0.6 to 0.8 expected).",
               f"- Needed: {br['need_mean']:.1f} N·m mean, {br['need_peak']:.1f} N·m peak at the 46 mm radius (C).",
               f"- The disc's {p['web_t']} mm web, {p['fr_ri'] - 38:.0f} mm wide, bends {p['fl_gap']} mm to reach the lining: {br['web_stress_nominal_mpa']:.0f} MPa and {br['web_force_nominal']:.0f} N of the clamp force (C); with the gap 0.1 mm larger, {br['web_stress_worst_mpa']:.0f} MPa and {br['web_force_worst']:.0f} N. 6061-T6 yields at 276 MPa and takes about 100 MPa for 10^8 cycles.",
               f"- Clamp force left on the lining: {br['clamp_net_n']:.0f} N.", ""]

    # ---------------------------------------------------------------- lost motion and accuracy
    as2um = lambda a, r: a/3600*math.pi/180*r*1e6
    enc = 360.0/MOTOR["counts"]/GEAR["ratio"]*3600                                                # arc-sec per encoder count at the output
    micro = MOTOR["step_deg"]*0.05/GEAR["ratio"]*3600                                             # 5 % step accuracy (D), at the output
    lm = [("Gear backlash (D: at most)", GEAR["backlash_arcsec"]), ("Gear hysteresis loss on torque reversal (D)", GEAR["hysteresis_arcsec"]),
          ("Wave generator on the motor shaft: key bonded with Loctite 638 (C: none)", 0.0),
          ("Friction joints rotor / bearing / nose / faceplate / vise (C: none below their slip torque)", 0.0),
          ("Closed-loop drive: +/- 1 encoder count (C)", 2*enc), ("Motor step accuracy 5 % of a step (D)", 2*micro)]
    worst = sum(a for _, a in lm); rss = math.sqrt(sum(a*a for _, a in lm))
    R["lost_motion"] = dict(items=lm, worst_arcsec=worst, rss_arcsec=rss, worst_um_rmax=as2um(worst, R_MAX), rss_um_rmax=as2um(rss, R_MAX),
                            accuracy_arcsec=GEAR["accuracy_arcsec"], accuracy_um_rmax=as2um(GEAR["accuracy_arcsec"], R_MAX),
                            repeat_uni_arcsec=2*enc + 5.0, repeat_uni_um_rmax=as2um(2*enc + 5.0, R_MAX), enc_arcsec=enc)
    md += ["## Lost motion, accuracy, repeatability\n", "| Source | arc-sec | µm at 46 mm | µm at 25 mm |", "|---|---|---|---|"]
    for n, a in lm: md.append(f"| {n} | {a:.1f} | {as2um(a, R_MAX):.1f} | {as2um(a, R_TYP):.1f} |")
    md += [f"| **Sum, worst case (reversing direction under load)** | {worst:.0f} | {as2um(worst, R_MAX):.1f} | {as2um(worst, R_TYP):.1f} |",
           f"| Root-sum-square | {rss:.0f} | {as2um(rss, R_MAX):.1f} | {as2um(rss, R_TYP):.1f} |",
           f"| Positioning accuracy: gear transmission error (D, cyclic and repeatable) | {GEAR['accuracy_arcsec']:.0f} | {as2um(GEAR['accuracy_arcsec'], R_MAX):.1f} | {as2um(GEAR['accuracy_arcsec'], R_TYP):.1f} |",
           f"| Repeatability approaching from one side: 2 counts + 5 arc-sec for the gear (G) | {2*enc + 5:.1f} | {as2um(2*enc + 5, R_MAX):.1f} | {as2um(2*enc + 5, R_TYP):.1f} |", ""]

    # ---------------------------------------------------------------- inertia, motor, speeds
    mp = {}
    for line in open(os.path.join(out, "build.out")):
        try: j = json.loads(line)
        except Exception: continue
        if j.get("tool") == "mass_properties" and "result" in j:
            for q in j["result"]["parts"]: mp[q["name"]] = q
    rotating = [n for n in mp if any(k in n for k in ("Faceplate", "Spindle nose", "Rotor", "Vise"))]
    J = sum(mp[n]["inertia_g_mm2"][0][0] for n in rotating)*1e-9                                   # kg m2 about x
    m_rot = sum(mp[n]["mass_g"] for n in rotating)/1000
    J += 0.5*0.45*(0.035**2 + 0.0275**2) if it > 1 else 0                                          # bearing inner ring (C, 0.45 kg)
    J += 0.5*0.35*0.03**2                                                                          # gear output flange + flexspline (G)
    J_work_typ = 0.5*(math.pi*0.02**2*0.1*2700)*0.02**2                                            # Ø40 x 100 aluminium
    J_work_max = 0.5*(math.pi*0.045**2*0.12*2700)*0.045**2                                         # Ø90 x 120 aluminium
    N_ = GEAR["ratio"]; Jm = MOTOR["j_rotor"] + GEAR["j_in"]
    mot = {}
    for nm, Jw in (("typ", J_work_typ), ("max", J_work_max)):
        Jo = J + Jw; refl = Jo/N_**2
        acc_out = 500.0*math.pi/180                                                                # rad/s2 asked for: 500 deg/s2
        t_acc = (Jm + refl)*acc_out*N_ + GEAR["no_load_start_nm"]
        t_cut_fin = L["fin_side"]*R_MAX/(N_*GEAR["eff"]); t_cut_rough = L["rough_side"]*R_MAX/(N_*GEAR["eff"])
        mot[nm] = dict(J_out=Jo, reflected=refl, ratio=refl/Jm, t_acc=t_acc, t_cut_fin=t_cut_fin, t_cut_rough=t_cut_rough,
                       margin_600=MOTOR["torque_at"][600]/(t_acc + t_cut_rough))
    R["motor"] = dict(J_rotating_no_work=J, mass_rotating=m_rot, cases=mot, steps_per_deg=MOTOR["counts"]*N_/360.0,
                      max_out_rpm_600=600/N_, max_deg_s=600/N_*6, accel_deg_s2=500.0, step_hz_at_max=MOTOR["counts"]*600/60.0)
    md += ["## Motor, speed, inertia\n",
           f"- Rotating group without a workpiece: {m_rot:.2f} kg, {J*1e6:.0f} kg·mm² about the axis (C, from the CAD). A Ø40 x 100 aluminium part adds {J_work_typ*1e6:.0f}, a Ø90 x 120 part {J_work_max*1e6:.0f} kg·mm².",
           f"- Reflected to the motor through 100:1: {mot['typ']['reflected']*1e7:.2f} g·cm² (typical part), {mot['max']['reflected']*1e7:.2f} g·cm² (largest part), against {Jm*1e7:.0f} g·cm² of rotor plus wave generator (D). Inertia ratio {mot['typ']['ratio']:.4f} to {mot['max']['ratio']:.4f}.",
           f"- Motor torque for 500 °/s² with the largest part: {mot['max']['t_acc']:.2f} N·m (C); for cutting through the gear at 70 % efficiency (G): {mot['max']['t_cut_fin']*1000:.0f} mN·m finishing, {mot['max']['t_cut_rough']*1000:.0f} mN·m roughing at 46 mm.",
           f"- Motor has 4.5 N·m holding (D) and {MOTOR['torque_at'][600]} N·m at 600 rpm (D: vendor curve at 60 V; a 48 V supply gives a little less at speed): margin {mot['max']['margin_600']:.1f}x over acceleration plus roughing.",
           f"- 600 motor rpm = {600/N_:.0f} rpm = {600/N_*6:.0f} °/s at the axis. {MOTOR['counts']*N_/360:.3f} steps per degree with the drive set to {MOTOR['counts']} pulses per rev; {MOTOR['counts']*600/60/1000:.0f} kHz step rate at top speed.", ""]

    # ---------------------------------------------------------------- natural frequencies
    nf = {}
    if "modal" in fea and "modes" in fea["modal"] and fea["modal"]["modes"][0]["frequency_hz"] > 1.0:
        nf["fea_modes_hz"] = [m["frequency_hz"] for m in fea["modal"]["modes"]]
    for nm, Jw in (("no part", 0.0), ("Ø40 x 100", J_work_typ), ("Ø90 x 120", J_work_max)):
        nf[f"torsion_gear_{nm}"] = math.sqrt(GEAR["k1"]/(J + Jw))/(2*math.pi)
        if k_brake: nf[f"torsion_brake_{nm}"] = math.sqrt((k_brake + GEAR["k1"])/(J + Jw))/(2*math.pi)
    # tilting of the rotating group on the bearing and the joints (rigid housing): J about the bearing centre
    mass_w = math.pi*0.02**2*0.1*2700
    J_tilt = 0.0
    for n in rotating:
        q = mp[n]; a = (q["cg_mm"][0] - x_brg)/1000.0
        J_tilt += q["inertia_g_mm2"][1][1]*1e-9 + q["mass_g"]/1000*a*a
    J_tilt_w = J_tilt + mass_w*(0.094 - x_brg/1000)**2 + mass_w*0.1**2/12
    k_series = 1/(1/k_tilt + sum(1/k for n, k in joints if "base" not in n))
    nf["tilt_lumped_no_part"] = math.sqrt(k_series/J_tilt)/(2*math.pi); nf["tilt_lumped_typ_part"] = math.sqrt(k_series/J_tilt_w)/(2*math.pi)
    if nf.get("fea_modes_hz") and ("overhang", "Fz") in defl:
        dd_ = defl[("overhang", "Fz")]
        nf["mode1_with_bearing_and_joints"] = nf["fea_modes_hz"][0]*math.sqrt(dd_["struct"]/dd_["total"])
    R["natural"] = nf
    md += ["## Natural frequencies\n", f"Tooth passing: 450 Hz at 9000 rpm, 1200 Hz at 24 000 rpm (3 flutes); spindle rotation 150 and 400 Hz.\n",
           "| Mode | Hz | Source |", "|---|---|---|"]
    for i, f in enumerate(nf.get("fea_modes_hz", [])): md.append(f"| Structure mode {i + 1} (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | {f:.0f} | F |")
    for k, v in nf.items():
        if k == "mode1_with_bearing_and_joints": md.append(f"| Structure mode 1 scaled for the bearing and joint compliance the FE model leaves out | {v:.0f} | C from F |")
        elif k != "fea_modes_hz":
            lab = (k.replace("torsion_gear_", "Axis turning on the reducer's spring, ").replace("torsion_brake_", "Axis turning against the brake, ")
                    .replace("tilt_lumped_no_part", "Rotating group tilting on the bearing and joints, no part")
                    .replace("tilt_lumped_typ_part", "Rotating group tilting on the bearing and joints, Ø40 x 100 part"))
            md.append(f"| {lab} | {v:.0f} | C |")
    md.append("")

    # ---------------------------------------------------------------- bearing life, static safety
    arm_o = (144.0 - x_brg)/1000
    if it > 1:
        M = L["rough_side"]*arm_o; P = phys.crb_equivalent_load(L["rough_side"], L["rough_axial"], M, RU85["dp"])
        life = phys.bearing_life_h(RU85["c_dyn"], P, 10.0)
        P0 = L["rough_peak"] + 2*L["rough_peak"]*arm_o/(RU85["dp"]/1000) + 0.44*L["rough_axial"]*DYN
        R["bearing_life"] = dict(P=P, M=M, L10_h_at_10rpm=life, static_safety=RU85["c0"]/P0, P0=P0)
        md += ["## Bearing\n", f"- Roughing at 100 mm past the jaws: moment {M:.0f} N·m, equivalent load {P:.0f} N (C) against C = {RU85['c_dyn']/1000} kN (D): L10 = {life:.2e} h at 10 rpm (C).",
               f"- Static: peak equivalent {P0:.0f} N against C0 = {RU85['c0']/1000} kN (D): safety {RU85['c0']/P0:.1f} (C).", ""]
    else:
        M = L["rough_peak"]*arm_o
        R["bearing_life"] = dict(M_peak=M, allow=GEAR["bearing_moment_allow"], share=M/GEAR["bearing_moment_allow"])
        md += ["## Bearing\n", f"- The gear unit's bearing sees {M:.0f} N·m at the roughing peak, 100 mm past the jaws; its allowable moment is {GEAR['bearing_moment_allow']} N·m (D): {M/GEAR['bearing_moment_allow']*100:.0f} %.", ""]

    # ---------------------------------------------------------------- bolts
    bl = []
    def joint(name, d, n, r_mm, grade, torque, mu=0.15):
        b = phys.bolt(d, torque, grade); cap = n*b["preload_n"]*mu*r_mm/1000
        bl.append(dict(joint=name, screws=f"{n} x M{d} {grade}", torque=torque, preload=b["preload_n"], proof_share=b["share_of_proof"], slip_torque=cap,
                       margin=cap/(L["rough_peak"]*R_MAX)))
    joint("vise to faceplate", 6, 4, 20.0, "12.9", 12.0)
    joint("faceplate to nose", 6, 6, 50.0, "8.8", 8.0)
    if it > 1:
        joint("nose to bearing inner ring", 5, 8, 32.5, "12.9", 7.0)
        joint("rotor to bearing inner ring", 5, 8, 32.5, "12.9", 7.0)
        joint("rotor to gear output flange", 8, 8, 21.0, "12.9", 30.0)
    else:
        joint("nose to gear output flange", 8, 8, 21.0, "12.9", 30.0)
    R["bolts"] = bl
    # vise prying: 4 x M6 on a 28.28 mm square, the vise's 54 mm width as the heel
    Mv = L["rough_peak"]*0.144; pre = phys.bolt(6, 12.0, "12.9")["preload_n"]
    pry = Mv/(2*(0.027 + 0.01414))                                                       # extra tension in the far pair, heel at the vise edge
    R["vise_pry"] = dict(moment=Mv, extra_tension=pry, preload=pre, share=pry/pre)
    md += ["## Bolted joints (slip against the roughing peak torque at 46 mm)\n", "| Joint | Screws | Tightening N·m | Preload each N | Share of proof | Slip torque N·m (µ 0.15) | Margin |", "|---|---|---|---|---|---|---|"]
    for b in bl: md.append(f"| {b['joint']} | {b['screws']} | {b['torque']} | {b['preload']:.0f} | {b['proof_share']*100:.0f} % | {b['slip_torque']:.0f} | {b['margin']:.1f}x |")
    md += ["", f"Vise prying: peak force 100 mm past the jaws puts {Mv:.0f} N·m on the vise base; the far pair of M6 sees {pry:.0f} N extra against {pre:.0f} N preload ({pry/pre*100:.0f} %): the joint stays closed (C).", ""]

    # ---------------------------------------------------------------- thermal
    dT = 10.0; R["thermal"] = dict(dT=dT, height_um=23.6e-6*p["H"]*dT*1000, length_um=23.6e-6*120*dT*1000)
    md += ["## Thermal growth\n", f"A {dT:.0f} K rise of the aluminium housing (G: motor at holding current) lifts the axis {R['thermal']['height_um']:.0f} µm and grows 120 mm of stack {R['thermal']['length_um']:.0f} µm (C).", ""]

    json.dump(R, open(os.path.join(out, "calc.json"), "w"), indent=1, default=float)
    open(os.path.join(out, "calc.md"), "w").write("\n".join(md))
    return R, md


if __name__ == "__main__":
    R, md = main(int(sys.argv[1]))
    print("\n".join(md))
