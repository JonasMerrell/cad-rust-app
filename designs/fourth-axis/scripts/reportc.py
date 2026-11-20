#!/usr/bin/env python3
"""Writes cycloidal/REPORT.md for the final cycloidal iteration from the files the other scripts left.

    reportc.py <n>
"""
import sys, os, json, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build6 import ROOT

AM = 180*60/math.pi


def main(n):
    cdir = os.path.join(ROOT, "cycloidal"); out = os.path.join(cdir, f"c{n}")
    J = lambda *a: json.load(open(os.path.join(*a)))
    meta = J(out, "meta.json"); p, x = meta["params"], meta["x"]; calc = J(out, "calc.json"); cy = J(cdir, "cycloid.json")
    dfm = J(out, "dfm.json") if os.path.exists(os.path.join(out, "dfm.json")) else {}
    mach = J(out, "machine", "results.json") if os.path.exists(os.path.join(out, "machine", "results.json")) else None
    c6 = J(ROOT, "iter6", "calc.json"); m6 = J(ROOT, "iter6", "machine", "results.json")
    ai = J(out, "assembly_info.json")
    mc = cy["monte_carlo"]; st = calc["stiffness"]; sp = calc.get("sprung", {}); mot = calc["motor"]; tp = calc.get("tool_point")
    k6 = c6["tool_point"]
    jaw = tp["chuck_Fz"]["total_um"] if tp else k6["chuck_Fz"]["total_less_bar"]*391e6
    jawf = jaw*78/391
    ovh = tp["overhang_Fz"]["total_um"] if tp else k6["overhang_Fz"]["total_less_bar"]*391e6
    probed = mc["probed and corrected, +/- 0.008 mm"]; careful = mc["careful, +/- 0.015 mm"]; asm = mc["as milled, +/- 0.03 mm"]; best = mc["best case, +/- 0.004 mm"]
    wf = [w for w in calc["windup"] if w["case"] == "finishing mean" and w["r_mm"] > 40][0]; wr = [w for w in calc["windup"] if w["case"] == "roughing peak"][0]
    n_inst = len(ai["instances"]); n_mach = sum(1 for q in meta["parts"] if q["kind"] == "machined"); n_ok = sum(1 for v in dfm.values() if v["ok"] and v["fits_travel"])
    L = []
    A = L.append
    A("# DMC2 rotary A axis with a self-made cycloidal reducer\n")
    A("Design only: nothing has been built, bought or run on the machine. Tags used for numbers: D vendor datasheet, C calculated, F rCAD finite elements, M Monte Carlo tolerance study, G guess. "
      f"The model is iteration c{n} in `c{n}/`; the bought strain-wave design it started from is in `../REPORT.md` (iterations 1 to 5) and `../iter6/`.\n")
    A("## 1. Result in short\n")
    A(f"- **What it is**: iteration 6's low headstock (steel faceplate with the XinDian H54-8-55 vise, RU66 crossed-roller main bearing, air brake, 120 mm tall, turned so the headstock stands at the right end of the table and the tool setter stays free) "
      f"with a {calc['ratio']['cycloid']:.0f}:1 cycloidal reducer cut on the DMC2 and a NEMA 23 closed-loop stepper beside the housing on a {calc['ratio']['belt']:.0f}:1 GT belt: {calc['ratio']['total']:.0f}:1 overall.")
    A(f"- **Size**: {abs(x['guard_r']) + 6 + 44:.0f} mm from the jaw tips to the back of the belt guard (iteration 6 with its NEMA 34 in line: 290 mm), 150 mm wide at the housing plus the motor to {148 if n >= 4 else 142} mm on the front side of the table, 120 mm tall.")
    A(f"- **Backlash**: zero up to the preload torque ({sp.get('preload_nm', 0):.0f} N·m, swinging +/- {sp.get('preload_swing_nm', 0):.1f} over a turn), by two discs sprung against each other through flexure blades in the output carrier. "
      f"Finishing at the largest radius needs {calc['torque']['fin_mean']:.1f} N·m. Roughing ({calc['torque']['rough_mean']:.0f} to {calc['torque']['rough_peak']:.0f} N·m) is done with the brake on.")
    A(f"- **Position error**: this is the weak point against the bought unit. With parts made to 0.008 mm (probed and corrected) the study gives {probed['te_pp_mean']:.1f} arc-min peak to peak "
      f"({probed['te_pp_mean']/2/AM*46.2e3:.0f} µm either way at the 46 mm radius, {probed['te_pp_mean']/2/AM*25e3:.0f} µm at 25 mm); as milled (0.03 mm) {asm['te_pp_mean']:.0f} arc-min. "
      "It repeats, so an error map in rcnc can take most of it out; how much is left has to be measured on the real unit (my estimate: about 1 arc-min). The strain-wave unit is 1 arc-min as bought.")
    A(f"- **Stiffness**: drive {st['K_drive']:.2e} N·m/rad (strain-wave unit 3.1e4 to 5.7e4, D). Windup when finishing at 46 mm radius {wf['um']:.1f} µm on the drive, {wf['um_brake']:.2f} µm with the brake; roughing peak with the brake {wr['um_brake']:.1f} µm (C).")
    A(f"- **Structure**: {jaw:.1f} µm at the jaws under the mean roughing force, {jawf:.1f} µm when finishing (targets 20 and 5); {ovh:.0f} µm 100 mm out without the tailstock ({'F, this model' if tp else 'F, iteration 6: same front end'}; bearing and joints added by hand, C/G).")
    A(f"- **Makeable**: {n_ok} of {n_mach} machined parts pass the one-cutter check (1/4 in end mill, two sides at most, no tapped holes, no hole under 6.5 mm). Steel parts: faceplate, nose, hubs, carriers, discs, pin plates, base.")
    if mach:
        must = [q for q in mach["poses"] if q["kind"] == "must be clear"]; bad = [q for q in must if any("base" not in h["a"] or "table" not in h["b"] for h in q["hits"])]
        A(f"- **On the machine**: {len(must) - len(bad) - 1} of {len(must)} required poses clear in the real machine model, tool setter touch-offs and routes with the 21 mm tool, the 24 mm tool and the 40 mm probe included; "
          "the one flagged pose is the machine model's own base against its own table at the end of Y travel, there without the A axis too (section 8).")
    A(f"- **Model**: {n_inst} placed parts, every screw, nut, pin and bearing; rCAD's interference check finds {len(ai.get('interference', []))} overlaps.\n")
    A("### Questions for Jonas\n")
    A("1. **Which measuring tools do you have?** Micrometer (ranges), pin gauges, gauge blocks or 1-2-3 blocks, dial test indicator (resolution), ring gauge or calibration ball, surface plate; and has the probe's repeatability been measured? This decides which row of the tolerance table is realistic.")
    A("2. Is there shop air for the brake (0.6 MPa assumed)? Without the brake the cycloidal drive should not rough.")
    A("3. The tool setter is taken at the back-left table corner (rcnc's X -5 / Y 289). The front-left corner was checked as geometry only, since it is outside the CAD's Y travel.")
    A("4. Prices in `bom.csv` are mostly my estimates; StepperOnline's pages refused an automated read today.\n")

    A("## 2. Why a cycloid, and what it costs\n")
    A("Asked for by Jonas (2026-11-20): make the cycloidal gearbox the main design, holding tighter tolerances by careful work, in-situ probing and correcting. "
      "A cycloidal reducer suits the mill: every part is a flat plate with holes and one outline, the tightest inside radius of the disc is "
      f"{cy['geometry']['min_concave_radius']:.2f} mm against the cutter's 3.175, and it is stiff and compact. What it costs is accuracy: the pitch lever is only e x N = {cy['geometry']['pitch_lever_mm']:.0f} mm, "
      "so 0.01 mm of error or play at the pins is 1.15 arc-min at the table. The bought strain-wave unit hides that problem inside a ground part.\n")
    A("## 3. Layout\n")
    A("Front to back (X along the axis, origin at the faceplate's front face):\n")
    rows = [("Vise, faceplate (steel), spindle nose (steel)", 44.0, x["ru_f"], "as iteration 6; four M6 through the main bearing's bore"),
            ("RU66 main bearing in plate P1 (31.75 aluminium)", x["p1_f"], x["p1_r"], "takes all cutting force; lining of the brake on P1's rear"),
            ("Hub disc A, brake disc, hub spacer (steel)", x["ru_r"], x["hubB_r"], "six M6 x 40 clamp them to carrier 1; the spacer runs in the lip seal"),
            ("C1 piston and seal plate (19.05 aluminium)", x["c_f"], x["c1_r"], "ring piston on two O-rings, air from the rear through a channel between two ring pins"),
            ("Carrier 1 (steel)", x["car1_f"], x["car1_r"], "front shaft bearing 6001, seats of the three A sleeves"),
            ("Pin plate G1 (steel)", x["g1_f"], x["g1_r"], "24 holes for the 8 mm ring pins"),
            ("Disc A, disc B on 6904 bearings and eccentric bushes", x["dA_f"], x["dB_r"], "identical discs, cut stacked, one turned half a turn"),
            ("Ring spacer M (aluminium)", x["m_f"], x["m_r"], "around the discs"),
            ("Pin plate G2 (steel)", x["g2_f"], x["g2_r"], "bored stacked with G1"),
            ("Carrier 2 spider (steel)", x["car2_f"], x["car2_r"], "three stiff arms clamp the A sleeves; three 2 mm blades carry the B sleeves"),
            ("Rear plate D with the motor ear (15.88 aluminium)", x["d_f"], x["d_r"], "rear shaft bearing 6001, windows for setting the preload, motor on slots"),
            ("Pulleys, belt, printed guard", x["d_r"], x["guard_r"], "GT3 3 mm, 20 and 60 teeth, 9 mm belt")]
    A("| Part | From x mm | To x mm | Note |"); A("|---|---|---|---|")
    for nm, a, b, note in rows: A(f"| {nm} | {a:.1f} | {b:.1f} | {note} |")
    A("")
    A(f"- Reducer: {p['N']} ring pins Ø{2*p['rp']:.0f} on Ø{2*p['Rp']:.0f}, {p['N'] - 1} lobes, eccentricity {p['e']} mm, discs {p['disc_t']} mm thick between R{cy['geometry']['disc_min_r']:.2f} and R{cy['geometry']['disc_max_r']:.2f}, "
      f"output through six Ø10 hardened sleeves on R{p['RO']:.0f} (each two needle-bearing inner rings on an M6), in holes of Ø{2*(p['rq'] + p['e'] + p['clear']):.2f}; the three holes a disc does not use are Ø{2*p['r_over']:.1f}.")
    A("- Input: a bought Ø12 hardened shaft with two eccentric bushes bonded on (bore and journal milled in one clamping), between a 6001 in carrier 1 and a 6001 in plate D.")
    A(f"- Motor 23E1KIP65-20 (IP65, 2 N·m, 4000 counts) on the +Y side, which on the machine is the front of the table where the spindle cannot reach anyway; its flange sits on a 6.56 mm web of plate D with slots for belt tension.")
    A("- Everything that locates is a flat face, a bore or a pocket; every screw has a nut in a milled pocket; four M8 x 120 tie rods clamp the plate stack; P1 and D are bolted down to the steel base from below.\n")
    A("**How the preload works.** Sleeves A sit in pockets of carrier 1 and drive disc A; sleeves B sit in pockets on the spider's blade pads and drive disc B. With the three A screws slack, the spider is turned against carrier 1 "
      "(two-pin spanner through windows in D) until disc A bears on one flank of the pins and disc B on the other and the blades are bent by the preload torque; then the A screws are tightened through the same windows. "
      "The blades follow the change of the gap round the turn, which a rigid lock cannot.\n")
    A("Pictures: `renders/assembly-iso.png`, `renders/assembly-back.png`, `renders/exploded.png`, `renders/section.png` (lengthwise), `renders/cross-section-disc.png` (through disc A), `renders/on-the-mill-*.png`.\n")

    A("## 4. Tolerance study\n")
    A("`scripts/cycloid.py`: planar rigid-body model, one disc on its eccentric inside the ring of pins, output through three sleeves in oversize holes. Per build it draws: every pin's position, a common error of the pin circle's radius, "
      "the disc outline's offset and a two-lobe waviness, the eccentric's throw and its bearing clearance, the holes' and sleeves' positions and sizes, each within the tolerance of the row. Both discs run on the same pins and are cut stacked. "
      "The clearance cut into the outline and the holes is the smallest at which fewer than 5 % of builds bind. 60 builds per row, 138 positions over a full table turn each.\n")
    A("| How well the parts are made | Clearance cut in mm | Lost motion, one disc, no preload (mean / 95 %) arc-min | Position error peak to peak (mean / 95 %) arc-min | of which once or twice per table turn / per input turn / pin passing | Change of the gap between the two discs over a turn (mean / 95 %) arc-min |")
    A("|---|---|---|---|---|---|")
    for k, v in mc.items():
        A(f"| {k} | {v['clearance']} | {v['lost_single_mean']:.1f} / {v['lost_single_p95']:.1f} | {v['te_pp_mean']:.1f} / {v['te_pp_p95']:.1f} | {v['te_out_pp_mean']:.1f} / {v['te_in_pp_mean']:.1f} / {v['te_rest_pp_mean']:.1f} | {v['split_residual_mean']:.1f} / {v['split_residual_p95']:.1f} |")
    A("")
    A("One arc-min is 13.4 µm at the 46 mm radius and 7.3 µm at 25 mm.\n")
    A("What it says:\n")
    A(f"1. As milled the drive is poor: {asm['lost_single_mean']:.0f} arc-min of lost motion without preload. My first estimate of 5 arc-min, given before this study, was wrong by a factor of seven.")
    A("2. Careful work helps in proportion: every column halves when the error halves. That is Jonas's point, and it holds.")
    A(f"3. Twisting the two discs against each other and locking them does not remove the play all the way round: the gap changes by {probed['split_residual_mean']:.0f} arc-min over a turn even for probed parts. Hence the sprung preload from c2 on.")
    A(f"4. Most of the position error is ripple as each pin takes over (552 times per table turn), and most of that comes from the clearance itself: perfect parts with 0.06 mm clearance already show {cy['perfect_with_clearance']['te_pp']:.1f} arc-min. "
      "So the clearance is kept as small as the measured parts allow, by choosing the pin diameter after the discs are measured (dowel pins or pin gauges in 0.01 mm steps).")
    A("5. Limits of the study: rigid parts and one pin driving at a time. Real parts give and share the load, which smooths the ripple; run-in wears the high spots. Neither is credited. The error distributions are my assumption (G); the real ones come from the machine checks in MAKING.md.\n")

    A("## 5. Iterations\n")
    A("| | What changed | Why | Result |"); A("|---|---|---|---|")
    A("| c1 | First full model: 23:1, two discs, six sleeves clamped between two solid carriers (twist and lock), aluminium pin plates, NEMA 23 on a 3:1 belt | starting point | 209 parts, no overlaps after three fixes (seal against carrier 1, disc outline facets); 20 of 21 parts machinable (a 1.5 mm² sliver on plate D by the motor ear) |")
    A(f"| c2 | Carrier 2 becomes a spider with three 2 mm flexure blades for the B sleeves; pin plates in steel; belt profile GT instead of HTD | the study showed a rigid lock leaves up to {probed['split_residual_p95']:.0f} arc-min of play somewhere in the turn; "
      f"steel discs in aluminium plates change the clearance {44*(23.6-11.7)*10/1000:.1f} µm per 10 K; an HTD belt's tooth clearance is 0.5 arc-min at the table | blade {sp.get('k_blade_fe') or 0:.0f} N/mm (F), preload {sp.get('preload_nm', 0):.0f} +/- {sp.get('preload_swing_nm', 0):.1f} N·m, blade stress {sp.get('max_stress_mpa', 0):.0f} MPa; spider machinable |")
    A("| c3 | Sliver on plate D removed; printed cover with three plugs for D's setting windows | machinability; grease in, chips out | 21 of 21 parts machinable; every required machine pose clear; but the modal run showed the motor nodding on its 6.56 mm web at 175 Hz |")
    A("| c4 | Motor flange in a pocket of plate D with walls toward the housing, above and outside, instead of on an open web | the 175 Hz motor mode | 255 Hz with the model's light motor block (about 200 Hz with the real motor): better, still the lowest mode; it does not reach the tool point. A printed wedge between the motor body and the housing's side (4.3 mm gap) would raise it further; not drawn |")
    A("")
    A("Two faults in my own checks were found and fixed on the way, and both changed numbers I had reported: (1) the machine check for the turned unit applied its half turn again at every pose, so every second pose had the unit facing the wrong way; "
      "(2) the structure FE runs named no material and rCAD then left one part out of the mesh, which made iteration 6 look softer than it is (16.7 µm at the jaws reported, 14.5 µm correct).\n")

    A("## 6. Calculations and simulation\n")
    L.extend(open(os.path.join(out, "calc.md")).read().split("\n")[4:])
    A("")
    A("## 7. Machinability\n")
    A("Each machined part sliced and tested: a 6.35 mm cutter (plus 0.1 mm) must reach everything that is not material from one face or the other, no deeper than its reach.\n")
    A("| Part | Size mm | Setups | Deepest cut mm | Largest cutter that would still do it mm | Result |"); A("|---|---|---|---|---|---|")
    for nm, v in dfm.items():
        A(f"| {nm} | {v['stock_xy_mm'][0]:.0f} x {v['stock_xy_mm'][1]:.0f} x {v['thickness_mm']:.2f} | {v['setups']} | {v['deepest_cut_mm']:.1f} | {v['largest_tool_mm']} | {'ok' if v['ok'] and v['fits_travel'] else 'FAIL'} |")
    A("")
    A("Not covered by the check: countersinks for the four M8 that come up through the base (90 degree chamfer mill, as iteration 6), and edge breaks. Steel parts are 1018 cold-rolled flat, O1 ground flat stock (discs) and 4140 pre-hardened or O1 (spider); how each is made is in `MAKING.md`.\n")

    A("## 8. On the machine\n")
    if mach:
        pl = mach["placement"]
        A(f"`scripts/machine_check6.py`: the assembly's STEP in the DMC2 machine model, turned half a turn about Z, faceplate at machine X {pl['x_faceplate']:.1f}, axis at Y {pl['y_axis']:.1f}, {pl['z_axis'] - 11.24:.2f} mm above the table; "
          "tool setter (Ø40, top 63.8 mm above the table) with its centre on the back-left table corner, and on the front-left corner as a second case; tools of 21, 24 and 40 mm (probe) below the collet nut. rCAD's interference check at every pose.\n")
        A("| Kind | Poses | Clear | Hits |"); A("|---|---|---|---|")
        kinds = {}
        for q in mach["poses"]: kinds.setdefault(q["kind"], []).append(q)
        for k, qs in kinds.items():
            hits = [q for q in qs if q["hits"]]
            A(f"| {k} | {len(qs)} | {len(qs) - len(hits)} | " + "; ".join(f"{q['name']}: " + ", ".join(sorted(set(h['a'] + ' x ' + h['b'] for h in q['hits'])))[:160] for q in hits[:6]) + " |")
        A("")
        A("The 'Y sweep at dy -150.7' hit is the machine model's own base against its own table at the end of Y travel; it is there without the A axis too.\n")
    else:
        A("(machine check not run for this iteration)\n")
    A("Rules that follow from the machine's shape (they apply to every variant):\n")
    A("- A retracted 24 mm tool's tip is 126 mm above the table and the unit is 120 mm tall, so the head can pass over the whole unit with Z up. The 40 mm probe's tip is 110 mm up: it passes beside the headstock, not over it.")
    A("- Small work close to the jaws is reachable: the top of a 20 mm part 14 mm past the jaw tips, and its side at centre height 50 mm out, are clear with the 24 mm tool at A 0 and A 90.\n")

    A("## 9. Making it\n")
    A("See `MAKING.md`: the accuracy each feature needs, the five machine checks that come first (reversal error, scale, squareness, probe calibration, run-out), the probe-and-correct loop, each part, assembly and preload setting, in-situ calibration and its limits, and the list of what rcnc needs.\n")

    A("## 10. Upgrade path to the bought strain-wave unit\n")
    A("If the cycloid's position error turns out not to be good enough, the front end stays: base, P1 with the RU66, faceplate, nose, brake, C1, hub disc A, brake disc, hub spacer, tie-rod pattern, tailstock. "
      "Taken out: carrier 1, G1, M, G2, the discs, shaft, spider, plate D with the motor. Put in: an adapter disc in carrier 1's place (six M6 nuts on the hub side, eight M8 clearance holes on a 42 mm circle into the HSS-25-100's output), "
      "iteration 6's spacer plate C2 and motor plate D around the reducer, and either the NEMA 34 in line or the same NEMA 23 on the belt (next section). The tie rods are the same four M8; the unit grows 36 mm in the first case. "
      "The adapter disc is not modelled; the rest is iteration 6 as built in `../iter6/`.\n")

    # ------------------------------------------------------------ comparison
    A("## 11. The three drives side by side\n")
    hs = c6["lost_motion"]; t6 = c6["torsion"]["table"]; w6f = [w for w in t6 if w["case"] == "finishing mean" and w["r_mm"] > 40][0]
    tb_peak = 36.2/(100*0.70)/0.95                                # belt 1:1 to the wave generator
    A("| | Strain wave, NEMA 34 in line (iteration 6) | Strain wave, NEMA 23 on a belt (numbers only) | Cycloid, NEMA 23 on a belt (c%s) |" % n); A("|---|---|---|---|")
    A(f"| Ratio | 100:1 | 100:1 with a 1:1 belt (150:1 with 20:30 teeth) | {calc['ratio']['total']:.0f}:1 |")
    A(f"| Length, jaw tips to rear end | 290 mm | about 190 mm | {abs(x['guard_r']) + 6 + 44:.0f} mm |")
    A(f"| Backlash | at most 20 arc-sec (D) | the same; the belt's 0.02 mm is 0.04 arc-sec at the table | zero below the {sp.get('preload_nm', 0):.0f} N·m preload; above it the brake is on |")
    A(f"| Lost motion on reversal under load | 60 arc-sec hysteresis (D) | the same | friction of the preloaded mesh; not calculated, to be measured |")
    A(f"| Position error | 60 arc-sec (D) | the same | {probed['te_pp_mean']*60:.0f} arc-sec peak to peak for probed parts (M); about 60 after mapping (G) |")
    A(f"| Torsional stiffness | 3.1e4 to 5.7e4 N·m/rad (D) | the same (belt reflected: 5e6) | {st['K_drive']:.1e} N·m/rad (C) |")
    A(f"| Windup, finishing at 46 mm, no brake | {w6f['um']:.1f} µm | the same | {wf['um']:.1f} µm |")
    A(f"| Motor torque margin at the roughing peak | {c6['motor']['cases']['typ'].get('margin_600', 0):.1f} at 600 rpm (mean roughing) | {1.21/tb_peak:.1f} at 300 rpm, {0.78/tb_peak:.1f} at 900 rpm | {mot['cases'][1]['m300']:.1f} at 300 rpm, {mot['cases'][1]['m900']:.1f} at 900 rpm |")
    A(f"| Table speed at 900 motor rpm | 54 deg/s | 54 deg/s (1:1) | {mot['deg_s_at']['900']:.0f} deg/s |")
    A("| Parts to make | 13 | 13 plus a pulley hub and a guard | %d |" % n_mach)
    A("| Bought drive parts | reducer 269 USD, NEMA 34 IP65 126, drive 46 | reducer 269, NEMA 23 IP65 and drive about 110 (est), belt and pulleys 35, bearing 10 | NEMA 23 IP65 and drive about 110 (est), bearings, pins, sleeves, shaft, belt and pulleys about 130 (est) |")
    A("| What can go wrong | pre-order lead time; the vendor's accuracy claim | as left, plus one more belt | everything in the tolerance table; it stands or falls with the making |")
    A("")
    A("**Belt to the strain-wave unit** (asked for as a variant, then set aside): sound on paper. A hub with a Ø20 journal in a 6004 bearing bolts to the wave generator's four M5; the NEMA 23 sits beside the housing as in the cycloid. "
      f"The motor needs {tb_peak:.2f} N·m at the roughing peak with a 1:1 belt and has 1.21 at 300 rpm (D), so no reduction is needed in the belt; everything the belt does wrong is divided by 100. It saves about 100 mm of length and 0.9 kg hanging off the back. "
      "It was not modelled further because Jonas moved the effort to the cycloid.\n")
    A("**My recommendation**: build the cycloid as the project it is meant to be, with the machine checks of MAKING.md done first; keep the brake and the front end exactly as drawn so that the strain-wave unit can go in later if the measured position error disappoints.\n")

    A("## 12. Review: what I am not sure of\n")
    for t in ["Position error after mapping (about 1 arc-min) is an estimate; the study itself gives %.1f arc-min before mapping for probed parts." % probed["te_pp_mean"],
              "Whether 0.008 mm is reachable depends on the DMC2's reversal error, squareness and the probe's repeatability, none of which I know (question 1).",
              "Efficiency 0.70 (G). If it is 0.5 the motor margin at the roughing peak falls below 1 without the brake; with the brake on, as intended, it does not matter.",
              "Radial stiffness of the 6904 bearings (1e8 N/m, G) sets the drive's stiffness; the figure could be 30 % either way.",
              "Blade preload: stiffness goes with the cube of the 2 mm blade thickness. Measure the blades; set the preload by torque, not by angle.",
              "The lip seal runs on a milled and polished steel rim at under 15 rpm; fine for grease, not for oil.",
              "Friction (stick-slip) of the preloaded mesh at very low speed is not modelled; it shows as a small reversal error that the brake does not remove.",
              "Soft steel discs on hardened pins will wear in. That helps evenness and costs preload; the preload is reset after run-in, and its swing is the wear indicator.",
              "The front 6001 sits in a blind pocket in carrier 1; to get it out again the pocket needs two small notches for a puller, not drawn.",
              "RU66 drawn from THK's dimension table (no vendor CAD); a clone bearing's accuracy is the vendor's claim.",
              "Shop air, brake friction coefficient and the lining are as iteration 5 and 6: assumptions until tried.",
              "The motor on its mount is the lowest mode (about 200 Hz with the real motor). The belt's tooth frequency crosses it at about 600 motor rpm; if it sings there, add the wedge between motor and housing.",
              "Prices are estimates."]:
        A("- " + t)
    A("")
    A("## 13. Files\n")
    A("| File | What |"); A("|---|---|")
    for f, w in [("REPORT.md", "this report"), ("MAKING.md", "making plan per part, probe-and-correct, calibration, rcnc requirement list"), ("bom.csv", "bill of materials for c%s" % n),
                 ("cycloid.md, cycloid.json, cycloid-profile.png", "tolerance study and the disc outline"), ("cycloid-pass2-independent-discs.md", "the same study with the discs cut one at a time and on separate pins (pessimistic)"),
                 ("c1/ to c4/", "each iteration: rCAD document a-axis.json, parts as STEP and PNG, renders, calc.md, dfm.json, FEA, machine check"),
                 ("renders/, step/", "pictures and STEP of the final iteration"),
                 ("../scripts/buildc.py, spider.py, cycloid.py, calcc.py, feac.py, spider_fea.py, extrasc.py, bomc.py, reportc.py", "everything is regenerated by these"),
                 ("../iter6/", "the low headstock with the bought strain-wave unit (comparison and upgrade path)")]:
        A(f"| `{f}` | {w} |")
    open(os.path.join(cdir, "REPORT.md"), "w").write("\n".join(L) + "\n")
    print("written", len(L), "lines")


if __name__ == "__main__":
    main(int(sys.argv[1]))
