#!/usr/bin/env python3
"""Hand calculations for a cycloidal iteration, on top of the tolerance study (cycloid.json), the structure numbers of
iteration 6 (same headstock front end) and this iteration's own FEA when it has been run.

    calcc.py <n>      -> cycloidal/c<n>/calc.json, calc.md

Tags:  D vendor datasheet,  C calculated here,  F rCAD finite elements,  M Monte Carlo (cycloid.py),  G guess / typical value.
"""
import sys, os, json, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import phys
from build6 import ROOT

AM = 180*60/math.pi            # rad -> arc-min
R_MAX, R_TYP = 0.0462, 0.025
E_ST = 210e9
MOTOR = dict(name="23E1KIP65-20", hold=2.0, j_rotor=4.8e-5, counts=4000, mass=1.2,                    # D: 2.0 N·m, 480 g·cm2, 1000-line encoder
             curve=[(100, 1.42), (200, 1.25), (300, 1.21), (400, 1.26), (500, 1.18), (600, 1.09), (700, 0.99), (800, 0.88), (900, 0.78), (1000, 0.74), (1200, 0.58), (1500, 0.53)])  # D: 36 V
B6904 = dict(C=6400.0, C0=3700.0, k=1.0e8)       # D (typical 6904 catalogue values); radial stiffness under load G
B6001 = dict(C=5100.0, C0=2370.0, k=6.0e7)       # D; stiffness G


def main(n):
    out = os.path.join(ROOT, "cycloidal", f"c{n}")
    meta = json.load(open(os.path.join(out, "meta.json"))); p = meta["params"]; x = meta["x"]
    cy = json.load(open(os.path.join(ROOT, "cycloidal", "cycloid.json")))
    c6 = json.load(open(os.path.join(ROOT, "iter6", "calc.json")))
    fea = json.load(open(os.path.join(out, "fea", "results.json"))) if os.path.exists(os.path.join(out, "fea", "results.json")) else None
    N, e = p["N"], p["e"]/1000; ratio = N - 1; belt = p["z2"]/p["z1"]; total = ratio*belt
    lever = e*N                                                   # m: pitch-point lever of the ring mesh
    Rres = {"iteration": f"c{n}", "ratio": dict(cycloid=ratio, belt=belt, total=total)}
    md = [f"# Cycloidal iteration c{n}: calculated results\n", "Tags: D datasheet, C calculated, F finite elements (rCAD), M Monte Carlo tolerance study, G guess.\n"]
    rough = phys.cutting(850, 9000); fin = phys.cutting(450, 24000)
    T = dict(rough_mean=rough["side"]*R_MAX, rough_peak=rough["side"]*2*R_MAX, fin_mean=fin["side"]*R_MAX, fin_peak=fin["side"]*2*R_MAX)
    Rres["torque"] = T
    md += ["## Drive train\n",
           f"- Cycloid {ratio}:1 ({N} ring pins Ø{2*p['rp']:.0f} on Ø{2*p['Rp']:.0f}, eccentricity {p['e']} mm), belt {p['z1']}:{p['z2']} = {belt:.0f}:1, total {total:.0f}:1 (C).",
           f"- Motor {MOTOR['name']}: {MOTOR['hold']} N·m holding, {MOTOR['counts']} counts per turn (D). One count = {360*3600/(MOTOR['counts']*total):.1f} arc-sec at the output (C).",
           f"- Cutting torque at the 46 mm radius: roughing {T['rough_mean']:.1f} mean / {T['rough_peak']:.1f} peak, finishing {T['fin_mean']:.1f} / {T['fin_peak']:.1f} N·m (C, as for the other iterations).", ""]

    # ---------------------------------------------------------------- torsional stiffness of the reducer
    L = p["disc_t"] - 0.55                                        # mm: contact length, disc thickness less edge breaks
    def mesh(Tq, pins=3):
        f = Tq/lever; q = f/pins/math.cos(math.radians(40))
        Rstar = 1/(1/p["rp"] - 1/6.0); Estar = 210e3/(2*(1 - 0.3**2))
        sig = math.sqrt(q*Estar/(math.pi*Rstar*L))
        dlt = 3.84e-5*q**0.9/L**0.8                               # mm, Palmgren
        kc = q/(2*dlt)*1000                                       # N/m: pin on disc and pin in its two holes, in series
        return f, q, sig, pins*kc*lever**2
    f36, q36, sig36, K_ring = mesh(T["rough_peak"])
    # output sleeves: Ø10 on a hole of Ø(10 + 2e + clearance); the sleeve is clamped at both ends by its M6
    RO = p["RO"]/1000
    fo = T["rough_peak"]/(RO*2.0)                                 # N per sleeve, two of three sharing (G)
    Rs = 1/(1/p["rq"] - 1/(p["rq"] + p["e"] + p["clear"])); Estar = 210e3/(2*(1 - 0.3**2))
    sig_o = math.sqrt(fo*Estar/(math.pi*Rs*L))
    dlt_o = 3.84e-5*fo**0.9/L**0.8; kco = fo/dlt_o*1000
    I_sl = math.pi*(0.010**4 - 0.007**4)/64 + math.pi*0.006**4/64 # sleeve + its screw
    Ls = p["sleeve_l"]/1000; a_ = (x["slA_f"] - (x["dA_f"] + x["dA_r"])/2)/1000   # load position from the seated end
    k_beam = 3*E_ST*I_sl*Ls**3/(a_**3*(Ls - a_)**3)               # clamped-clamped beam, point load at a
    K_out = 2.0*(1/(1/kco + 1/k_beam))*RO**2
    # eccentric: 6904 on its bush, the 12 mm shaft between the two 6001 bearings
    xa = (x["bush_f"] + x["bushA_r"])/2; xf = x["car1_r"] + 4.0; xr = x["d_r"] + 4.0
    span = (xf - xr)/1000; a1 = (xf - xa)/1000; b1 = span - a1
    I_sh = math.pi*0.012**4/64
    k_shaft = 3*E_ST*I_sh*span/(a1**2*b1**2)
    k_sup = 1/((b1/span)**2/B6001["k"] + (a1/span)**2/B6001["k"])
    k_ecc = 1/(1/B6904["k"] + 1/k_shaft + 1/k_sup)
    K_ecc = k_ecc*lever**2
    K_one = 1/(1/K_ring + 1/K_out + 1/K_ecc)
    # belt: stiffness of the two spans, reflected to the output
    r2 = p["z2"]*3/math.pi/2/1000; span_b = math.sqrt((p["belt_c"]/1000)**2 - (r2 - p["z1"]*3/math.pi/2/1000)**2)
    EA = 9.0*4000.0                                               # N per unit strain: 3 mm GT, glass cords, 9 mm wide (G: about 4 kN per mm of width)
    K_belt = 2*EA/span_b*r2**2*ratio**2
    K_drive = 1/(1/K_one + 1/K_belt)
    kb = c6["torsion"]["k_brake"]
    Rres["stiffness"] = dict(K_ring=K_ring, K_out=K_out, K_ecc=K_ecc, k_ecc_n_per_m=k_ecc, k_shaft=k_shaft, K_one_disc=K_one, K_belt_at_output=K_belt, K_drive=K_drive, K_brake=kb,
                             pin_load_n=q36, mesh_force_n=f36, hertz_ring_mpa=sig36, hertz_sleeve_mpa=sig_o, sleeve_force_n=fo)
    md += ["## Torsional stiffness (one disc carrying the torque)\n",
           "| Element | N·m/rad | Tag |", "|---|---|---|",
           f"| Ring mesh: 3 pins sharing, steel on steel line contact | {K_ring:.3g} | C |",
           f"| Output sleeves in their holes, sleeve bending | {K_out:.3g} | C |",
           f"| Eccentric: 6904 ({B6904['k']:.0e} N/m, G), Ø12 shaft over a {span*1000:.0f} mm span ({k_shaft:.2e} N/m, C), two 6001 | {K_ecc:.3g} | C/G |",
           f"| **Reducer, one disc** | **{K_one:.3g}** | C |",
           f"| Belt (both spans, 9 mm GT, at the output) | {K_belt:.3g} | G |",
           f"| **Drive, motor shaft held** | **{K_drive:.3g}** | C |",
           f"| Brake (iteration 6: same disc and lining) | {kb:.3g} | C |", "",
           f"For comparison the bought HSS-25 strain-wave unit: 3.1e4 to 5.7e4 N·m/rad (D). " + ("With the sprung preload the second disc adds only its blades' stiffness, so this figure holds in both directions up to the preload torque." if p.get("sprung") else f"Below the preload torque both discs share and the reducer is about twice as stiff ({2*K_one:.3g})."), "",
           "| Case | Radius mm | Torque N·m | Windup, drive only: arc-min | µm at the surface | With the brake on: µm |", "|---|---|---|---|---|---|"]
    wind = []
    for nm, Tq, r in (("roughing mean", T["rough_mean"], R_MAX), ("roughing peak", T["rough_peak"], R_MAX), ("roughing mean", T["rough_mean"]*R_TYP/R_MAX, R_TYP),
                      ("finishing mean", T["fin_mean"], R_MAX), ("finishing mean", T["fin_mean"]*R_TYP/R_MAX, R_TYP)):
        w = Tq/K_drive; wb = Tq/(K_drive + kb)
        wind.append(dict(case=nm, r_mm=r*1000, torque=Tq, arcmin=w*AM, um=w*r*1e6, um_brake=wb*r*1e6))
        md.append(f"| {nm} | {r*1000:.0f} | {Tq:.1f} | {w*AM:.2f} | {w*r*1e6:.1f} | {wb*r*1e6:.2f} |")
    Rres["windup"] = wind
    md += ["", f"Contact stress at the roughing peak ({T['rough_peak']:.0f} N·m on one disc): ring pins {sig36:.0f} MPa, output sleeves {sig_o:.0f} MPa (C). "
           "Annealed O1 or 1018 takes about 1000 MPa in line contact before it dents (G: 2.5 to 3 times yield).", ""]

    # ---------------------------------------------------------------- bearings on the input side
    def ecc_load(Tq): return 1.3*Tq/lever                         # N: mesh resultant plus the sleeves' share (G: factor 1.3)
    P_mean = ecc_load(T["rough_mean"]); P_pk = ecc_load(T["rough_peak"]); n_in = 10*ratio
    L10 = (B6904["C"]/P_mean)**3*1e6/(n_in*60)
    Ff = P_mean*b1/span
    Rres["bearings"] = dict(ecc_mean_n=P_mean, ecc_peak_n=P_pk, static_safety=B6904["C0"]/P_pk, L10_h_at_10rpm_roughing=L10, front_6001_n=Ff)
    md += ["## Eccentric and shaft bearings\n",
           f"- Load on a disc's 6904 bearing: {P_mean:.0f} N at the mean roughing torque, {P_pk:.0f} N at its peak (C, 1.3 x mesh force). Static safety {B6904['C0']/P_pk:.1f} (C0 {B6904['C0']:.0f} N, D).",
           f"- Life at 10 rpm of the table, roughing all the time: {L10:,.0f} h (C). Not a limit.",
           f"- Shaft: the mean roughing load bends the Ø12 shaft {P_mean/k_shaft*1e6:.1f} µm at the eccentric (C); front 6001 carries {Ff:.0f} N.", ""]

    # ---------------------------------------------------------------- motor margin, speed, inertia
    eta_c, eta_b = 0.70, 0.95                                      # G: greased sliding sleeves and pins rolling in their holes
    pre = p.get("preload_nm", 5.0)
    t_pre = 2*pre*(1 - eta_c)/(ratio*belt)                         # both discs loaded against each other: friction of two meshes, at the motor
    Jout = c6["motor"]["J_rotating_no_work"] + 0.0012               # carriers, hub spacer (C: steel discs R38 x 22 mm)
    Jin = MOTOR["j_rotor"] + 1.0e-6 + (2.3e-5 + 2*1.1e-5)/belt**2    # rotor + small pulley + (big pulley + eccentrics) reflected
    acc = math.radians(500.0)
    t_acc = (Jin*total + Jout/total)*acc
    rows = []
    for nm, Tq in (("roughing mean", T["rough_mean"]), ("roughing peak", T["rough_peak"]), ("finishing mean", T["fin_mean"])):
        tm = Tq/(total*eta_c*eta_b) + t_pre
        rows.append((nm, Tq, tm))
    def avail(rpm):
        c = MOTOR["curve"]
        for (a, ta), (b, tb) in zip(c[:-1], c[1:]):
            if a <= rpm <= b: return ta + (tb - ta)*(rpm - a)/(b - a)
        return c[0][1] if rpm < c[0][0] else c[-1][1]
    md += ["## Motor\n", "| Case | Output N·m | At the motor N·m | Margin at 300 rpm | at 600 rpm | at 900 rpm |", "|---|---|---|---|---|---|"]
    mot = []
    for nm, Tq, tm in rows:
        mot.append(dict(case=nm, out=Tq, motor=tm, m300=avail(300)/tm, m600=avail(600)/tm, m900=avail(900)/tm))
        md.append(f"| {nm} | {Tq:.1f} | {tm:.2f} | {avail(300)/tm:.1f} | {avail(600)/tm:.1f} | {avail(900)/tm:.1f} |")
    back_eff = 2 - 1/eta_c
    Rres["motor"] = dict(cases=mot, eta_cycloid=eta_c, eta_belt=eta_b, preload_nm=pre, preload_friction_at_motor=t_pre, t_acc=t_acc, J_out=Jout, J_in=Jin,
                         deg_s_at=dict([(r, r*6/total) for r in (300, 600, 900)]), back_efficiency=back_eff, hold_needed_peak=T["rough_peak"]*back_eff/total)
    md += ["", f"- Efficiency {eta_c:.2f} for the reducer (G) and {eta_b:.2f} for the belt (G); the {pre:.0f} N·m preload between the discs costs {t_pre:.2f} N·m at the motor (C).",
           f"- Table speed: {300*6/total:.0f} / {600*6/total:.0f} / {900*6/total:.0f} deg/s at 300 / 600 / 900 motor rpm (C). Accelerating at 500 deg/s² takes {t_acc:.3f} N·m (C).",
           f"- Back-drivable: efficiency backwards about {back_eff:.2f} (C from the forward guess), so the motor must hold {T['rough_peak']*back_eff/total:.2f} N·m against the roughing peak; it has {MOTOR['hold']} (D). "
           "With air and power off the table can be turned by hand with a few N·m.", ""]

    # ---------------------------------------------------------------- resonance of the drive
    nat = {}
    for nm, Jw in (("no part", 0.0), ("Ø40 x 100 aluminium", 0.5*2700*math.pi*0.02**2*0.1*0.02**2), ("Ø90 x 120 aluminium", 0.5*2700*math.pi*0.045**2*0.12*0.045**2)):
        nat[nm] = dict(drive=math.sqrt(K_drive/(Jout + Jw))/(2*math.pi), preloaded=math.sqrt(1/(1/((1 if p.get("sprung") else 2)*K_one) + 1/K_belt)/(Jout + Jw))/(2*math.pi), brake=math.sqrt((K_drive + kb)/(Jout + Jw))/(2*math.pi))
    Rres["natural"] = nat
    md += ["## Torsional natural frequency of the table on the drive\n", "| Work | One disc Hz | Both discs (below preload) Hz | Brake on Hz |", "|---|---|---|---|"]
    md += [f"| {k} | {v['drive']:.0f} | {v['preloaded']:.0f} | {v['brake']:.0f} |" for k, v in nat.items()]
    md += ["", "Tooth passing is 450 Hz at 9000 rpm and 1200 Hz at 24 000 rpm with the three-flute cutter.", ""]

    # ---------------------------------------------------------------- lost motion and accuracy
    mc = cy["monte_carlo"]; md += ["## Lost motion and accuracy (tolerance study, 60 builds per row)\n",
                                   "| How well the parts are made | Clearance cut in mm | Lost motion, one disc, no preload: mean / 95 % arc-min | Position error p-p: mean / 95 % arc-min | of which slow / per input turn / pin passing | Play left somewhere in the turn with the two discs twisted and locked: mean / 95 % arc-min | The same as µm at 46 mm |", "|---|---|---|---|---|---|---|"]
    for k, v in mc.items():
        md.append(f"| {k} | {v['clearance']} | {v['lost_single_mean']:.1f} / {v['lost_single_p95']:.1f} | {v['te_pp_mean']:.1f} / {v['te_pp_p95']:.1f} | {v['te_out_pp_mean']:.1f} / {v['te_in_pp_mean']:.1f} / {v['te_rest_pp_mean']:.1f} | "
                  f"{v['split_residual_mean']:.1f} / {v['split_residual_p95']:.1f} | {v['split_residual_mean']/AM*R_MAX*1e6:.0f} / {v['split_residual_p95']/AM*R_MAX*1e6:.0f} |")
    belt_bl = 0.02e-3/r2/ratio                                     # GT profile: about 0.02 mm of tooth clearance on the large pulley (G)
    enc = 2*math.pi/(MOTOR["counts"]*total)
    Rres["lost_motion"] = dict(monte_carlo=mc, belt_arcmin=belt_bl*AM, encoder_count_arcmin=enc*AM)
    md += ["", f"- Belt: a GT-profile belt has about 0.02 mm of tooth clearance (G): {belt_bl*AM:.2f} arc-min at the output. An HTD belt (0.1 mm) would give {0.1e-3/r2/ratio*AM:.2f}; so GT, not HTD.",
           f"- One encoder count is {enc*AM*60:.1f} arc-sec (C).",
           "- The study is rigid-body: every pin is taken as hard and only one pin drives at a time. Real parts give a little and share the load, which smooths the pin-passing part; I have not credited that.", ""]

    # ---------------------------------------------------------------- sprung preload
    if p.get("sprung"):
        sp = os.path.join(out, "fea", "spider.json"); kh = p["blade_k_n_per_mm"]
        fe = json.load(open(sp)) if os.path.exists(sp) else None
        k_bl = (fe["k_n_per_mm"] if fe else kh[1])                      # N/mm at the seat, bending
        k_eff = 1/(1/k_bl + 1/kh[2])                                    # with the blade's twist under the load's axial offset (C)
        KB = 3*k_eff*1e3*RO**2                                          # N·m/rad between the carriers
        gap95 = mc["probed and corrected, +/- 0.008 mm"]["split_residual_p95"]/AM   # rad: how much the gap between the discs changes over a turn
        swing = KB*gap95/2
        fmax = (pre + swing)/(3*RO); vm = (fe["vm_per_n"] if fe else 2.4)*fmax
        Rres["sprung"] = dict(k_blade_fe=fe["k_n_per_mm"] if fe else None, k_blade_hand=kh, k_eff=k_eff, K_between_carriers=KB, preload_nm=pre, preload_swing_nm=swing,
                              deflection_arcmin=pre/KB*AM, max_force_n=fmax, max_stress_mpa=vm)
        md += ["## Sprung preload (carrier 2 spider)\n",
               f"- One blade, load at the sleeve seat: {k_bl:.0f} N/mm" + (" (F: 128 000 elements, 1.2 mm)" if fe else " (C)") + f"; hand estimate {kh[1]:.0f} (C). With the blade's twist under the offset load: {k_eff:.0f} N/mm (C).",
               f"- Between the carriers: {KB:.0f} N·m/rad (C). The {pre:.0f} N·m preload bends the blades {pre/KB*AM:.1f} arc-min ({pre/KB*RO*1e3:.3f} mm at the sleeves).",
               f"- The gap between the discs changes by up to {gap95*AM:.1f} arc-min over a turn for probed parts (M, 95 %), so the preload swings {pre:.0f} +/- {swing:.1f} N·m. "
               f"It never drops below the finishing torque ({T['fin_mean']:.1f} N·m at the 46 mm radius)." if pre - swing >= T["fin_mean"] else f"- WARNING preload swing {swing:.1f} of {pre:.0f} N·m.",
               f"- Highest blade stress at the top of the swing: {vm:.0f} MPa" + (" (F)" if fe else " (C)") + ". 4140 pre-hardened yields at about 650 MPa, annealed O1 at about 400 (D, typical).",
               f"- Torque in the direction disc A carries: full stiffness to any load. In the other direction: both discs up to about the preload, then disc B's blades give until disc A's other flank takes over, "
               f"so beyond the preload there is lost motion of the size in the table above. Roughing is done with the brake on; finishing stays below the preload.", ""]

    # ---------------------------------------------------------------- thermal
    dT = 10.0; a_al, a_st = 23.6e-6, 11.7e-6
    g_mat = a_st if p.get("pins_g") == "Steel" else a_al
    dclear = p["Rp"]*(g_mat - a_st)*dT*1000                         # µm radial: pin circle grows more than the steel disc
    Rres["thermal"] = dict(dT=dT, clearance_change_um=dclear, arcmin=dclear/1000/(lever*1000)*AM*2 if dclear else 0.0)
    md += ["## Thermal\n",
           f"- Pin plates of {'steel' if g_mat == a_st else 'aluminium'}, discs of steel: a {dT:.0f} K rise changes the radial clearance at the pins by {dclear:.1f} µm (C)"
           + (f", which is {dclear/1000/(lever*1000)*AM*2:.1f} arc-min of extra play or lost preload. Too much next to a 0.01 mm fit: the pin plates should be steel." if dclear else ": the reducer is all steel where it matters."),
           f"- Axis height and length grow as iteration 6 (15 µm and about 28 µm per 10 K, C).", ""]

    # ---------------------------------------------------------------- structure
    if fea and "head" in fea:
        md += ["## Deflection at the tool point (this iteration's FE model, bonded, every part aluminium)\n", "| Load point | Dir | Structure F µm | Iteration 6 structure µm | Total with bearing and joints, roughing µm | Finishing µm |", "|---|---|---|---|---|---|"]
        tp = {}
        for tag in ("chuck", "overhang"):
            for dr in ("Fz", "Fy"):
                s = fea["head"][tag]["static"][dr]["tool"]["magnitude_mm"]*1000
                k6 = c6["tool_point"][f"{tag}_{dr}"]; s6 = k6["struct"]*391e6
                tot = (k6["total_less_bar"] - k6["struct"])*391e6 + s      # iteration 6's bearing and joints (same parts), the test bar's own bending taken out
                tp[f"{tag}_{dr}"] = dict(struct_um=s, iter6_struct_um=s6, total_um=tot, finishing_um=tot*78/391)
                md.append(f"| {tag} | {dr} | {s:.1f} | {s6:.1f} | {tot:.1f} | {tot*78/391:.2f} |")
        Rres["tool_point"] = tp; md.append("")
        if "modal" in fea and "modes" in fea["modal"]:
            Rres["modes"] = fea["modal"]["modes"]
            fr = [m_["frequency_hz"] for m_ in fea["modal"]["modes"] if m_["frequency_hz"] > 1.0]
            md += ["Natural frequencies of the bonded model with a 100 mm test bar (F, 7 mm mesh, each part its own material; brake disc, hub spacer and carrier 1 as one cylinder): "
                   + ", ".join(f"{f_:.0f}" for f_ in fr) + f" Hz. The first ({fr[0]:.0f} Hz) is the motor nodding on its mount, modelled as a 0.72 kg block; with the real 1.2 kg it is about {fr[0]*math.sqrt(0.72/1.2):.0f} Hz (C). "
                   "It does not move the tool point. The modes that do are the test bar bending on the spindle: " + ", ".join(f"{f_:.0f}" for f_ in fr[2:4]) + " Hz, as in iteration 5. "
                   "The run also returns one mode at 0 Hz with no mass in it: a sliver the coarse mesh left unattached.", ""]
    else:
        k6 = c6["tool_point"]
        md += ["## Deflection at the tool point\n", "Front end identical to iteration 6 (same faceplate, nose, RU66, P1, base); iteration 6's figures apply until this iteration's own FE run is in:",
               f"{k6['chuck_Fz']['total_less_bar']*391e6:.1f} µm at the jaws when roughing, {k6['overhang_Fz']['total_less_bar']*391e6:.1f} µm 100 mm out without the tailstock.", ""]
    b = c6["brake"]
    md += ["## Brake (unchanged from iteration 6)\n", f"Clamp {b['clamp_n']:.0f} N at 0.6 MPa, holding {b['torque_mu03']:.0f} to {b['torque_mu06']:.0f} N·m for µ 0.3 to 0.6; needed {b['need_mean']:.0f} mean / {b['need_peak']:.0f} peak.", ""]
    json.dump(Rres, open(os.path.join(out, "calc.json"), "w"), indent=1)
    open(os.path.join(out, "calc.md"), "w").write("\n".join(md))
    print("\n".join(md))


if __name__ == "__main__":
    main(int(sys.argv[1]))
