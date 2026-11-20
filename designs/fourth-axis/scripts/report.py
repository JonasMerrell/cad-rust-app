#!/usr/bin/env python3
"""Writes REPORT.md from the iteration folders (calc.json, dfm.json, machine/results.json) and the text below.

    report.py <final iteration>
"""
import sys, os, json, math
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build import params, stack, ROOT

FINAL = int(sys.argv[1]) if len(sys.argv) > 1 else 5


def load(it, name):
    p = os.path.join(ROOT, f"iter{it}", name)
    return json.load(open(p)) if os.path.exists(p) else None


def um(c, f): return c*f*1e6


def key_numbers(it):
    c = load(it, "calc.json")
    if not c: return None
    L = c["loads"]; tp = c.get("tool_point", {})
    g = lambda k, f: (tp[k][f]*L["rough_side"]*1e6 if k in tp else None)
    out = dict(chuck=g("chuck_Fz", "total_less_bar"), over=g("overhang_Fz", "total_less_bar"), chuck_fin=(tp["chuck_Fz"]["total_less_bar"]*L["fin_side"]*1e6 if "chuck_Fz" in tp else None))
    t = c["torsion"]["table"][0]
    out["wind"] = t["um"]; out["wind_brake"] = t.get("um_brake")
    out["tail"] = None
    if "tailstock" in c:
        v = c["tailstock"]["parts"].get("d25_L120")
        if v: out["tail"] = v["with_tail_mid"]*L["rough_side"]*1e6
    nat = c.get("natural", {})
    out["mode1"] = (nat.get("fea_modes_hz") or [None])[0]; out["mode1c"] = nat.get("mode1_with_bearing_and_joints")
    if out["mode1"] is not None and out["mode1"] < 1.0: out["mode1"] = out["mode1c"] = None      # a part came loose in the coarse modal mesh: no result
    return out


def fmt(v, d=1): return "n/a" if v is None else f"{v:.{d}f}"


def main():
    it = FINAL; p = params(it); x = stack(p); c = load(it, "calc.json"); L = c["loads"]
    dfm = load(it, "dfm.json") or {}; mach = load(it, "machine/results.json") or {}
    kn = {i: key_numbers(i) for i in range(1, it + 1)}
    H = p["H"]; XFP = 51.0
    motor_rear = XFP + x["motor_r"]; jaw = XFP + 44; tail_max = 215.0
    br = c.get("brake", {}); lm = c["lost_motion"]; mot = c["motor"]; nat = c["natural"]; bl = c.get("bearing_life", {})
    tpf = c["tool_point"]
    R = []
    A = R.append
    A("# DMC2 rotary A axis: design report\n")
    A("Written 2026-11-20. Everything here is modelled and calculated; nothing has been built, bought or run. Tags used throughout: "
      "**D** vendor datasheet, **C** calculated in `scripts/calc.py`, **F** rCAD finite elements, **G** guess or typical value.\n")
    A("## 1. Result in short\n")
    A(f"- **What it is:** a rotary A axis along X that bolts to the M8 fixture grid in place of the two-station vise. A bought size-25 strain-wave reducer (100:1) drives a "
      f"spindle that runs in a bought 120 mm crossed-roller bearing; a pneumatic disc brake clamps the spindle for indexed roughing; a closed-loop NEMA 34 stepper drives it. "
      f"The XinDian H54-8-55 self-centring vise sits on a removable faceplate with its clamp centre on the axis. A plain tailstock with a dead centre supports long parts.")
    A(f"- **Envelope:** centre height {H:.1f} mm above the table. The vise swings Ø92 mm. Parts up to about Ø90 mm can be machined on top; the tool reaches down to centre height on parts up to about Ø66 mm "
      f"(the collet nut is the limit, section 8). Between the jaw face and the tail centre: up to about {tail_max - jaw:.0f} mm. The vise grips 8 to 55 mm.")
    A(f"- **Stiffness at the mean roughing force ({L['rough_side']:.0f} N):** {fmt(kn[it]['chuck'])} µm at the jaws (target 20). Finishing force: {fmt(kn[it]['chuck_fin'], 2)} µm (target 5). "
      f"At 100 mm overhang without the tailstock it is {fmt(kn[it]['over'])} µm, so long parts need the tailstock ({fmt(kn[it]['tail'])} µm mid-length on a Ø25 x 120 aluminium bar).")
    A(f"- **Torsion:** with the brake on, {fmt(kn[it]['wind_brake'])} µm at the 46 mm radius while roughing. On the reducer alone, {fmt(kn[it]['wind'])} µm roughing and 5.3 µm finishing at 46 mm (1.6 µm on a Ø50 part).")
    A(f"- **Backlash:** the reducer's rating is 20 arc-sec at most (D), with 60 arc-sec of hysteresis on a torque reversal (D). Approaching from one side, repeatability is about {lm['repeat_uni_arcsec']:.0f} arc-sec "
      f"({lm['repeat_uni_um_rmax']:.1f} µm at 46 mm). No keys, splines or gears of our own are in the torque path: every joint we make is a clamped flat face.")
    A(f"- **Makeable:** {sum(1 for v in dfm.values() if v['ok'])} of {len(dfm)} machined parts pass an automatic check for one 1/4 in end mill from two sides (section 9). No tapped holes, no holes under 6.5 mm, no press fits. Two parts are printed without supports.")
    A("- **Not verified:** nothing is measured. The bearing's stiffness, the bolted joints, the brake's friction and the shop air pressure are calculated or assumed (section 12).\n")
    A("### Decisions and questions for Jonas\n")
    A("1. **It replaces the vise.** The two-station vise covers 318 of the table's 350 mm, so the two cannot share the table.")
    A("2. **Air:** the brake needs about 6 bar and one controller output. If that is not wanted, the same disc can be clamped by hand through the port (not designed here), or the axis runs on the reducer alone with roughing kept to smaller radii.")
    A("3. **Cost:** the reducer is 269 USD (under the 300 USD line). The separate main bearing adds about 180 USD for a P5 clone, 700 to 820 USD for a real THK. Without it (iteration 1) the axis is about ten times softer at the jaws.")
    A("4. **NEMA 34, not NEMA 23:** the size-25 reducer takes a 14 mm keyed shaft, which is the NEMA 34 shaft. It needs the CL86T drive. The mill's own axes are NEMA 23 by the CAD.")
    A(f"5. **Tool measuring with the axis mounted:** the headstock stands {H + p['top']:.0f} mm above the table and a retracted 24 mm tool tip only {126:.0f} mm, so a loaded tool cannot cross the headstock. rcnc needs a keep-out and a route (section 11).")
    A(f"6. **Left tub wall:** the motor ends {abs(motor_rear) - 127.2:.0f} mm past the table's left edge, {199.2 - abs(motor_rear):.1f} mm short of the wall in the CAD. Measure the real gap before buying.")
    A("7. **Fixture grid:** the grid's hole positions are not in the CAD. The base plates use 25 mm spacings, so they land on either grid, but the axis position is only known to +/- 12.5 mm.\n")

    A("## 2. What was measured from the machine model\n")
    A("From `~/.config/cad-rust-app/machines/DMC2.json` and the original STEP (model coordinates, mm):\n")
    A("| Item | Value |\n|---|---|")
    A("| Table | 350 x 250, x -127.2 to 222.8, y -475.3 to -225.3, top z 11.24 |")
    A("| Travel | X -123.7 to 219.3 (343), Y -150.7 to 83.1 (234), Z -135.3 to 14.7 (150) |")
    A("| Spindle (collet nut face) | x 0, y -310.07, z 146.54 at the modelled position; 150 mm above the table at Z top |")
    A("| Tool tip above the table at Z top | 126 mm with the usual 24 mm stick-out |")
    A("| Spindle reach over the table in Y | only the back 168 mm (table y -393 to -225) |")
    A("| Two-station vise | 318 x 145 mm, jaw tops 28 mm above the table |")
    A("| Tub walls | inner faces at x -199.2 and 294.8 above the table top |")
    A("| Fixture grid | not modelled; one vise screw sits at x 8.85, y -287.8 |\n")

    A("## 3. Loads\n")
    A("Forces come from rCAD's own cutting model (Kienzle; `docs/cutting-model.md`): tangential force = power / cutting speed, sideways force on the tool and work = tangential x 1.3 x sqrt(1 + 0.35²). "
      "`cutsim/cutsim.py` runs rCAD's stock simulation on plain side cuts with the 1/4 in 3-flute tool: 385 N at 835 W and 9000 rpm, 127 N at 736 W and 24 000 rpm, the same ratios.\n")
    A("| Case | Tangential N | Sideways mean N | Peak N | Axial N | Torque at 46 mm, mean / peak N·m |\n|---|---|---|---|---|---|")
    A(f"| Roughing, 850 W at 9000 rpm | {L['rough_ft']:.0f} | {L['rough_side']:.0f} | {L['rough_peak']:.0f} | {L['rough_axial']:.0f} | {L['rough_side']*0.0462:.1f} / {L['rough_peak']*0.0462:.1f} |")
    A(f"| Finishing, 450 W at 24 000 rpm | {L['fin_ft']:.0f} | {L['fin_side']:.0f} | {L['fin_peak']:.0f} | {L['fin_axial']:.0f} | {L['fin_side']*0.0462:.1f} / {L['fin_peak']*0.0462:.1f} |\n")
    A(f"- **Dynamic factor 2** on the mean for interrupted cutting (G, argued): a straight-tooth cut 1 mm wide and 6 mm deep peaks at {L['straight_tooth_peak_over_mean']:.1f} times its mean, "
      "but the 38 degree helix spreads each tooth over about 85 degrees at 6 mm depth, so successive teeth overlap. Two is on the safe side for depths of 5 mm and more.")
    A("- **Worst direction assumed:** the whole sideways force is taken tangential to the work at its largest radius for torque, and square to the axis for bending.")
    A("- **For scale:** rCAD's model says the tool itself bends 0.17 mm at the roughing load with 24 mm stick-out. The axis targets (20 and 5 µm) are far tighter than the cutter.\n")

    A("## 4. Concept comparison\n")
    A("| Concept | Lost motion | Torsional stiffness N·m/rad | Holding torque N·m | Cost USD | Precision needed from parts made on this mill |\n|---|---|---|---|---|---|")
    A("| **Bought strain-wave unit, size 25, 100:1, crossed-roller output (chosen)** | backlash 20 arc-sec or less, hysteresis 1 arc-min (D) | 3.1 to 5.7 x10^4 (D) | 67 rated, 157 repeated peak, 284 momentary (D) | 269 (D) | flat faces; concentricity 0.1 mm (D, vendor manual) |")
    A("| Same, size 17 (fits a NEMA 23 directly) | same | 1.0 to 1.6 x10^4 (D) | 24 / 54 / 108 (D) | 245 (D) | same |")
    A("| Cycloidal drive machined here | about 5 arc-min with +/- 0.03 mm lobes and pin circle (C from G) | 2 to 5 x10^4 (G) | high | about 60 | +/- 0.005 mm profiles for 1 arc-min: not achievable |")
    A("| Two-stage preloaded belt, 25:1 | none in the belts; about 13 arc-sec from the motor (C) | about 1.1 x10^4 (C from Gates belt data) | 50, set by the motor | about 60 | low, but a 127 mm output pulley and no stiffness to spare |")
    A("| Split or spring-loaded worm (bought table, motorised) | 0.2 deg stock on a Sherline (D); 1 to 2 arc-min adjusted (G) | about 1 x10^5 (G) | self-locking | 550 (D, Vertex HV-4) | a worm cannot be cut with an end mill; spring loading gives way above its preload |")
    A("| Direct drive, 110 mm torque motor | none | servo stiffness only | 5 to 8 continuous (D): too little | 1,766 and up (D) | low, but not a stepper and far over budget |\n")
    A("Why the strain-wave unit: it is the only option that puts all the precision in a bought part and still costs under 300 USD. All four sizes cost within 50 USD of each other, so the stiffest one that fits was taken. "
      "The datasheets and STEP are StepperOnline's; ratings match the Laifual and Harmonic Drive tables line for line, and the stiffness values are Harmonic Drive's for the same size (Laifual publishes none for 100:1).\n")
    A("What the reducer alone cannot do: hold 20 µm at the largest radius while roughing (24.6 µm), or carry bending on its own bearing (iteration 1). Hence the separate bearing and the brake.\n")

    A("## 5. The design (iteration %d)\n" % it)
    A("Frame: X along the axis, origin on the axis at the faceplate's front face. Front to back:\n")
    A("| Part | What it does | Made from |\n|---|---|---|")
    A("| Vise H54-8-55 | workholding; base on the faceplate, jaws pointing along +X, 4 x M6 from behind | bought |")
    A("| Faceplate, Ø118 x 12.7 | carries the vise; comes off the nose with six M6 from the front, so the vise can be changed without opening the axis | 6061 plate |")
    A("| Spindle nose, Ø110 | bolts to the bearing's inner ring (8 x M5 from the front); captive M6 nuts in open slots for the faceplate | 6061 plate |")
    A("| RU85 crossed-roller bearing | the spindle bearing: both rings have their own bolt holes, so there are no fits to hold | bought |")
    A("| P1 bearing plate, 31.75 | bearing recess in front, brake recess and friction lining behind | 6061 plate |")
    A("| Rotor with brake disc | joins the reducer's output flange to the inner ring; its rim is the brake disc on a 1 mm web | 6061 plate |")
    A("| Brake piston + 2 O-rings | ring piston in a groove in plate C, pushes the disc rim onto the lining | 6061 plate, bought O-rings |")
    A("| C cylinder plate, 38.1 | spacer round the reducer body, brake cylinder, air port | 6061 plate |")
    A("| HSS-25-100 reducer | positions the spindle; its flange is trapped in plate D | bought |")
    A("| D motor plate, 31.75 | clamps the reducer, carries the motor; shimmed off plate C | 6061 plate |")
    A("| Motor 34E1KIP65-45 | IP65 closed-loop stepper, shaft straight into the wave generator | bought |")
    A("| Base plate, 12.7 | joins the plates to the table: 4 x M8 down into the grid, 4 x M8 up into nuts in pockets milled in the plates' faces | 6061 plate |")
    A("| Splash ring, sensor bracket | cover the nose gap; hold the home sensor | printed ASA |")
    A("| Tailstock plate + base + 12 mm spot drill | dead centre in a pinch clamp; slides on slots, any length | 6061 plate, bought |\n")
    A("How the constraints shaped it:\n")
    A("- **No threads, no small holes.** A 1/4 in end mill cannot make a tap drill hole. Every thread is in a bought part (bearing rings, reducer, vise) or is a nut. Nuts sit in pockets milled into a plate's face, "
      "including the nuts that hold the standing plates down on the base: a slot for the bolt and a pocket for the nut, cut from the face, replace a hole drilled into the plate's edge.")
    A("- **No fits to hold.** The bearing and the reducer locate on bolted flat faces with clearance round them. They centre themselves when the rotor is bolted up first and the tie bolts last. The only bores that matter are the reducer and motor pilots in plate D, and the vendor allows 0.1 mm there.")
    A("- **Two bearings on one spindle.** The reducer has its own output bearing. So that it never fights the main bearing, plate D is bolted to plate C over shims chosen at assembly to fill the measured gap, and the rotor's joints are flat faces that take up any offset before they are tightened.")
    A("- **Brake.** The disc is clamped between a ring piston (two O-rings in step glands, 0.3 mm stroke, no sliding fits) and a lining bonded to plate P1. The disc's 1 mm web bends 0.15 mm to close the gap; in its own plane it is about 20 times stiffer than the reducer.")
    A("- **Coolant.** IP65 motor; the reducer's input is sealed by its own O-ring against plate D; the plate joints get RTV; the brake cavity is closed; the bearing has its own seals behind a printed splash ring with a drain; the home sensor is a potted M8 inductive type.\n")

    A("## 6. Iteration history\n")
    A("| Iteration | What changed | Why | At the jaws, rough µm | 100 mm out, no tail, µm | Ø25 x 120 with tail, µm | Windup at 46 mm, rough µm (brake on) | First mode Hz, FE / with bearing and joints |\n|---|---|---|---|---|---|---|---|")
    why = {1: ("Baseline: the reducer's own bearing is the spindle; one 19 mm plate; no brake, no tailstock", "simplest thing that could work"),
           2: ("Separate RU85 main bearing; three bolted plates (31.75 / 38.1 / 31.75) on a base with two rows of bolts; rotor; tailstock", "iteration 1 bent 113 µm at the jaws and loaded the reducer's bearing to 89 % of its limit"),
           3: ("Disc brake (piston in plate C, lining on P1), air port, home sensor", "the reducer alone winds up 24.6 µm at 46 mm when roughing"),
           4: ("Brake disc on a 1 mm web with a narrower friction ring; shims between C and D; air tube bonded into C; splash ring; home sensor on a printed bracket in front; shorter tail centre; lower tie bolts raised",
               "iteration 3's 2.5 mm disc would yield (478 MPa); the reducer was pinched between two bearings; the sensor did not fit the base; coolant could reach the bearing"),
           5: ("Table bolts moved; bearing lands under P1 and D, C lifted off the base; pocket corners R3.6; tailstock clamp pockets and slit a 6.35 cutter can make; reducer turned 4.5 degrees",
               "adversarial review: a hex key could not reach two table bolts past the tie bolts; plate edges cut from two sides would rock; three features could not be milled")}
    for i in range(1, it + 1):
        k = kn[i] or {}
        wb = f"{fmt(k.get('wind'))} ({fmt(k.get('wind_brake'), 2)})" if k.get("wind_brake") is not None else f"{fmt(k.get('wind'))} (no brake)"
        A(f"| {i} | {why[i][0]} | {why[i][1]} | {fmt(k.get('chuck'))} | {fmt(k.get('over'))} | {fmt(k.get('tail'))} | {wb} | {fmt(k.get('mode1'), 0)} / {fmt(k.get('mode1c'), 0)} |")
    A("\nEach iteration is a full rCAD model with every screw (`iter<N>/a-axis.json`), built by `scripts/build.py <N>`, and has its own `calc.md`, `fea/` and renders. "
      "Iteration 1 has no modal result: in the coarser modal mesh one of its thin parts came loose (0 Hz modes), and it was not worth chasing for a design already ruled out. "
      "The brake-on windup of iterations 4 and 5 is larger than 3's because the disc's web is thinner; it is the price of a disc that does not yield.\n")

    A("## 7. Simulation results, iteration %d\n" % it)
    A("How the numbers are made:\n")
    A("- **Structure (F):** rCAD's FE solver, 10-node tetrahedra, 5 mm mesh (about 500 000 nodes), parts bonded where their faces meet, base plate's underside fixed. A 40 x 40 test bar is gripped in the vise and loaded at its end. "
      "The static runs use rCAD's iterative solver, which takes one material for every part (aluminium), so the steel vise is modelled about three times too soft. One run with real materials quantifies that.")
    A("- **Bearing (C):** the RU85 as 36 rollers of 6 x 4.8 mm at 45 degrees with Palmgren's line-contact law and 2 µm preload, then halved for ring and seat flexing. Roller count and size are guessed to fit the catalogue's static rating; the halving is a guess.")
    A("- **Bolted joints (G):** each joint as a contact layer of 2000 N/mm³. Published values run 500 to 5000, so this column could be half or double.")
    A("- **Check of the solver:** a 10 x 10 x 100 cantilever came out at 0.5816 mm against 0.5806 by beam theory, and its first mode at 814 Hz against 816.\n")
    body = open(os.path.join(ROOT, f"iter{it}", "calc.md")).read()
    body = body.split("## Deflection at the tool point", 1)[1]
    A("### Deflection at the tool point" + body.replace("\n## ", "\n### "))
    A("\nReading the results against the targets:\n")
    A(f"- **At the jaws:** {fmt(kn[it]['chuck'])} µm roughing, {fmt(kn[it]['chuck_fin'], 2)} µm finishing. Both targets are met.")
    A(f"- **100 mm past the jaws with no tailstock:** {fmt(kn[it]['over'])} µm roughing. Not met, and it cannot be with a 120 mm bearing: the bearing's tilt alone is about 19 µm there. Use the tailstock, or rough at half the load.")
    A("- **Brake stiffness source:** iteration 3's 2.5 mm disc was solved by FE (1.7 x10^6 N·m/rad). The 1 mm web of iterations 4 and 5 would not solve (the solver overflowed on a mesh fine enough for it), so its value is the ring formula with the hub's share taken from that FE result: 7.4 x10^5 N·m/rad (C).")
    A("- **Windup:** met with the brake on. On the reducer alone, roughing at the full 46 mm radius is 24.6 µm (over) and finishing is 5.3 µm (at the limit); on a Ø50 part both are met.")
    A(f"- **Resonance:** on the reducer alone the axis's torsional mode is {nat.get('torsion_gear_Ø90 x 120', 0):.0f} to {nat.get('torsion_gear_no part', 0):.0f} Hz, which brackets the 450 Hz tooth-passing frequency at 9000 rpm. "
      "Continuous 4-axis cutting at 9000 rpm with a heavy cut could excite it; at 24 000 rpm (1200 Hz) it is well clear. With the brake on the mode is above %.0f Hz. "
      "The first bending mode of the head carrying a 40 x 40 x 100 steel bar with no tailstock is %s Hz in the bonded FE model and about %s Hz once the bearing and joints are counted (C from F): "
      "close to 450 Hz, so a heavy bar that long should not be roughed at 9000 rpm without the tailstock." % (min(v for k, v in nat.items() if k.startswith("torsion_brake")), fmt((nat.get("fea_modes_hz") or [None])[0], 0), fmt(nat.get("mode1_with_bearing_and_joints"), 0)))
    A("- **Motor:** six times the torque needed for 500 °/s² plus roughing. Top speed is set by the stepper: 600 rpm is 36 °/s.")
    A("- **Thermal:** a 10 K rise lifts the axis 18 µm. Probe the centre height again once the motor is warm, and let the drive cut the current at standstill.\n")
    A("Evidence files: `iter%d/fea/results.json`, `fea-chuck-Fz.png`, `fea-overhang-Fz.png`, `fea-tail-Fz.png`, `fea-brake-torsion.png`, `fea-mode1.png` to `fea-mode3.png`, `calc.json`.\n" % it)

    A("## 8. On the machine: interference\n")
    if mach:
        pl = mach["placement"]
        A(f"rCAD's assembly interference check with the real machine STEP (trunnion left out), the A axis riding with the table, a 6.35 x 24 mm tool under the collet nut. "
          f"Placement: faceplate front at x {pl['x_faceplate']:.0f}, axis at y {pl['y_axis']:.1f}, z {pl['z_axis']:.2f}. `scripts/machine_check.py`, document `iter{it}/machine/on-machine.json`.\n")
        A("| Position | Axis offsets X / Y / Z | Kind | Result |\n|---|---|---|---|")
        for q in mach["poses"]:
            hit = "; ".join(f"{h['a']} x {h['b']} ({h['volume_mm3']:.0f} mm³)" for h in q["hits"][:3]) or "clear"
            A(f"| {q['name']} | {q['dx']:+.1f} / {q['dy']:+.1f} / {q['dz']:+.1f} | {q['kind']} | {hit} |")
        A("")
    A("What it shows:\n")
    A("- **The unit against the frame:** clear over the whole Y travel, head parked at either end.")
    A("- **Retracted tool:** clear over the motor, the vise and the work. It is **not** clear over the plates and the faceplate: they stand higher than a retracted 24 mm tool. That zone (left of the faceplate) is a keep-out with a tool loaded. The collet nut itself clears the plates by 8 mm.")
    A("- **Route to the left end with a tool in:** go behind the unit first (table forward), then along X. Clear.")
    A("- **Cutting on top of a Ø90 part:** clear from the jaws to the tail centre.")
    A("- **Reach down the side:** the collet nut (Ø22) and the spindle body limit it, not the axis. With 24 mm stick-out the tool gets to centre height beside parts up to about Ø66 (C: nut radius 11 against a part of radius r with the tip 3 mm below centre gives r <= 33). Bigger parts are cut within about 20 mm of the top and turned.")
    A("- **Close to the jaws:** with the vise upright (A 90) keep the tool 12 mm or more from the jaw face, or the nut meets the vise.\n")

    A("## 9. Making each part on the mill\n")
    A("All machined parts are 6061 plate, cut with the 1/4 in 3-flute end mill (T24) from one or two sides; edges are broken with the chamfer mill. `scripts/dfm.py` slices each part and checks that every cut is reachable by a 6.45 mm circle "
      "from one of two opposite faces, no deeper than the flute (19.05 mm) or, stepped, the stick-out (24 mm), and that the part fits the travel.\n")
    A("| Part | Size mm | Setups | Deepest cut mm | Largest tool that would still fit mm | Check |\n|---|---|---|---|---|---|")
    for nm, v in dfm.items():
        A(f"| {nm} | {v['stock_xy_mm'][0]:.0f} x {v['stock_xy_mm'][1]:.0f} x {v['thickness_mm']:.2f} | {v['setups']} | {v['deepest_cut_mm']:.1f}{' (stepped, past the flute)' if v['deeper_than_flute'] else ''} | {v['largest_tool_mm']} | {'pass' if v['ok'] and v['fits_travel'] else 'FAIL'} |")
    A("\nNo CAM has been written. Notes for whoever writes it (the CAM guidelines apply):\n")
    A("| Part | Stock | How it is held and cut | What must be right, and how to check it |\n|---|---|---|---|")
    A("| Base plate | 1/2 in plate, 215 x 160 | Clamp to the fixture plate on a spoil sheet; top side: M8 counterbores and all through holes, outline. Flip: counterbores for the bolts that come up, located from two of the holes | Thickness as supplied. Hole spacing 175 x 125 for the grid |")
    A("| P1 bearing plate | 1-1/4 in plate, 160 x 135 | Rear side first: face, brake recess, nut slots, bolt holes, nut pockets, outline to 19 mm deep (this band is the land that stands on the base). Flip onto the faced side, locate from the tie holes: face, bearing recess 15 deep, bore, rest of the outline 0.2 mm under size | Recess depth 15.00 +0.05 (depth mic); recess Ø120.10 +0.05: the bearing drops in by hand. Brake recess 4.45 +/- 0.03 deep. Faces parallel within 0.02 (indicator on the table) |")
    A("| C cylinder plate | 1-1/2 in plate | Front: face, piston groove Ø93.00 -0.05 / Ø122.40 +0.05 x 6.00 deep, bore and outline half way. Flip: face, bore and outline to meet, port hole | Groove walls smooth (finish pass, climb, full depth): they seal against the O-rings. Thickness 38.1 as faced: note it, the rotor shim takes up the difference |")
    A("| D motor plate | 1-1/4 in plate | Front: face, reducer recess Ø107.4 x 10.0, pilot bore Ø67.03 +0.03 to 12 mm, nut slots. Flip, locate from the tie holes: motor pilot Ø73.10 x 2, rest of the bore, nut pockets, outline land | Reducer pilot: the reducer pushes in by hand. Front and rear pilots concentric within 0.1 mm (the vendor's limit): indicate the front bore after the flip |")
    A("| Rotor with brake disc | 1 in plate, 130 x 130 | Rear first: face, Ø54 boss, M5 counterbores, all holes, friction face. Flip onto a spoil plate with a pocket for the boss, bolt through the M8 holes: hub, then the web pocket last in light cuts | Web 1.0 +/- 0.1 (measure through a hole with a ball mic or by depth). Friction faces flat |")
    A("| Brake piston | 1/4 in plate | Double-sided tape or tabs on a spoil sheet; rear: the two O-ring steps; flip: 0.6 relief | Step diameters Ø97.4 +0.05 and Ø118.0 -0.05. It must slide in the groove with the O-rings oiled |")
    A("| Spindle nose | 5/8 in plate | Rear: face leaving the 2 mm boss, nut slots, holes. Flip: face, M5 counterbores, outline | Boss and front face parallel within 0.02 |")
    A("| Faceplate | 1/2 in plate | Rear: face, vise counterbores, holes. Flip: face, M6 counterbores, target pin hole, outline | Both faces milled, parallel within 0.02: it sets how true the vise runs |")
    A("| Tailstock plate | 1 in plate | Rear: face, pinch channel and pockets, nut pockets, bore Ø12.02, slit, outline land. Flip: face, rest of outline | Bore: the spot drill slides in and the pinch bolt locks it |")
    A("| Tailstock base | 3/4 in plate | Top: slots, holes, outline. Flip: counterbores | none |")
    A("| Friction lining | P400 silicon carbide paper | Cut a ring Ø108 to Ø122 with a knife; bond to the P1 recess floor with slow epoxy under a flat weight, after the bearing screws are in | Even, thin glue line; no glue on the grit |")
    A("| Splash ring, sensor bracket | ASA | Printed flat, no supports | none |\n")
    A("Fixturing that has to be made first: a spoil plate with an M8 pattern for the rotor and the discs. The big plates are bolted through their own tie holes after a first clamped operation; none is taller than 38 mm on the table.\n")

    A("## 10. Assembly and alignment\n")
    A("Order (every screw can be reached at its step; checked against the model):\n")
    A("1. **P1:** drop the eight M5 nuts into their slots in the brake recess and tape them. Set the RU85 in its recess, eight M5 x 20 from the front, 7 N·m. Then bond the lining over the nuts.")
    A("2. **Rotor:** eight M5 x 20 from the rear into the inner ring, 7 N·m, with medium threadlocker. Measure from P1's rear face to the disc rim and to the lining with a depth micrometer: the gap must be 0.10 to 0.25 mm. Correct it with a shim ring between rotor and inner ring.")
    A("3. **C:** oil the two O-rings, fit them and the piston in the groove. Put the two M8 nuts in P1's pockets. RTV on the joint, C onto P1, tie bolts in loosely.")
    A("4. **D unit:** nuts for the motor in D's slots. Reducer into D's recess, two M5 x 30 from behind. Wave generator onto the motor shaft with its key and Loctite 638 as the vendor's manual says; grease; motor onto D with four M6 x 30.")
    A("5. **Join:** offer the D unit up. Eight M8 x 20 through the bearing's bore into the reducer's output flange, 30 N·m, while D is still free to find its place. Measure the gap between C and D at each tie bolt with feeler gauges, fill each with shim washers, then tighten the six M8 x 120 to 20 N·m. Seal the gap with RTV. Bond the air tube into C.")
    A("6. **Base:** four M8 x 25 up through the base into the nuts in P1 and D, 12 N·m (the nuts bear on aluminium). Sensor bracket from below.")
    A("7. **Front:** splash ring on with RTV, drain down. Nose: eight M5 x 12. Vise onto the faceplate from behind (four M6 x 16), faceplate onto the nose (six M6 x 16, 8 N·m).")
    A("8. **On the table:** four M8 x 16 into the grid, finger tight.\n")
    A("Alignment:\n")
    A("1. **Vise on centre.** Grip a ground pin, turn A, indicate the pin near the jaws. Tap the vise until it runs within 0.01 mm, then tighten its screws through the faceplate (take the faceplate off and put it back; it returns on its face, so re-check).")
    A("2. **Axis parallel to X, sideways.** Indicator in the spindle against the side of the pin, traverse X over 80 mm. Swing the unit about one table bolt (the holes have 0.5 mm play) until it reads within 0.01, tighten.")
    A("3. **Axis parallel to X, up and down.** Same on top of the pin. Correct with shim stock under the front or rear pair of table bolts.")
    A("4. **Centre height and Y centre.** Probe the pin's top and both sides: Y centre is the middle, centre height is the top minus half the pin. Store both in rcnc. Repeat warm.")
    A("5. **A zero.** Probe a flat held in the vise at two X positions, or the jaw tops; set A0.")
    A("6. **Tailstock.** Slide it up, grip the spot drill, bring its point to a centre mark made at the axis (or indicate the cone while turning A with an indicator on the faceplate). Tap the base sideways and shim for height, then tighten.")
    A("7. **Brake.** With 6 bar on, a torque wrench on a bar in the vise must not turn the spindle at 45 N·m. Release: the spindle must turn freely by hand through the motor (drive off).\n")
    A("Cables and air: the motor's two 1 m leads leave toward the back; run them and the 6 mm air line in a loop along the table's back edge to the left wall, long enough for the 234 mm of Y travel. The sensor cable runs along the base's front edge and joins them.\n")

    A("## 11. What rcnc needs (not implemented)\n")
    A("| Item | Value |\n|---|---|")
    A(f"| Steps per degree | {mot['steps_per_deg']:.3f} (drive at 4000 pulses per rev x 100 / 360) |")
    A(f"| Top speed | 36 °/s (2160 °/min, 600 motor rpm, 40 kHz steps); the reducer allows 3500 rpm average input |")
    A("| Acceleration | 500 °/s² |")
    A("| Home | M8 inductive sensor, NPN normally open, 24 V, on a steel pin in the faceplate; seek at 10 °/s, latch at 1 °/s, always from the same side |")
    A("| Brake | one output to a 3/2 valve; wait 0.3 s after switching; never step A with the brake on; brake on by default while 3-axis moves cut, off for A moves |")
    A("| Positioning | finish every A move in the same direction (overshoot 1 degree and come back) to take out the reducer's 1 arc-min hysteresis |")
    A("| Feed | inverse-time or a feed along the surface for moves with A; A is a rotary axis that wraps |")
    A("| Keep-out | with the A axis mounted, no travel with a tool in the spindle over the box from the faceplate leftwards and the unit's width, at any Z; the tool-change and tool-setter route goes behind the unit |")
    A("| Work offsets | A zero, the axis's Y centre and centre height, from the probing steps above |")
    A("| Drive alarm | the CL86T's alarm output to a feed hold: a closed-loop stepper that loses position must stop the program |\n")

    A("## 12. Risks and assumptions\n")
    A("| # | Assumption or risk | Kind | What it changes | How to settle it |\n|---|---|---|---|---|")
    A("| 1 | RU85 stiffness (0.7 x10^6 N·m/rad tilt) is calculated from guessed roller data and halved | C / G | the bearing column of every deflection | measure: load a bar in the vise with a known weight under an indicator |")
    A("| 2 | Bolted joints at 2000 N/mm³ | G | the joints column, 2 to 9 µm | same test |")
    A("| 3 | A clone RU85 at 180 USD has the stated P5 runout and preload | G | runout of the vise; stiffness if it has clearance | buy from a seller who states C0 clearance; measure on arrival |")
    A("| 4 | Reducer stiffness is Harmonic Drive's table for the same size, not the maker's own | D (proxy) | windup on the reducer alone | measure windup with a lever and indicator |")
    A("| 5 | Brake friction 0.3 to 0.6 on bonded abrasive paper, 6 bar available | G | holding torque 37 to 74 N·m against 36 needed at the peak | the 45 N·m torque-wrench test at assembly |")
    A("| 6 | Brake gap 0.10 to 0.25 mm after stacking six tolerances | C | web stress 80 to 200 MPa, clamp force | measured and shimmed at step 2 |")
    A("| 7 | The 1 mm web survives the cut and the duty | C | brake | 6061 at 120 MPa nominal; cut it last, supported; inspect after the first 1000 cycles |")
    A("| 8 | Brake piston returns by O-ring roll-back and the disc's spring | G | drag when released | check free rotation; add three light springs under the piston if it drags |")
    A("| 9 | The vise is drawn from two vendor drawings; thread depths, mass (1.09 kg listed) and the Ø9 bores are not confirmed | D / G | screw lengths, inertia | measure the vise in hand before cutting the faceplate |")
    A("| 10 | Left tub wall 72 mm from the table as in the CAD; motor rear ends 4 mm short of it | D (CAD) | whether it fits at all | measure the machine |")
    A("| 11 | Fixture grid phase unknown; table holes assumed M8 tapped through | G | exact position (+/- 12.5 mm) | measure one hole |")
    A("| 12 | Torsional mode of the axis on the reducer alone sits near 450 Hz | C | chatter in continuous 4-axis roughing at 9000 rpm | rough indexed with the brake; pick the spindle speed away from it; rcnc's listening guard |")
    A("| 13 | Headstock taller than a retracted tool | F | a crash if a loaded tool crosses it | rcnc keep-out and route (section 11) |")
    A("| 14 | Tailstock is a dead centre in a pinch clamp, 10 to 13 N/µm, and it forces the part's end to where the point is | F | accuracy of long parts | make the centre mark with the part running on the axis; a bought live-centre tailstock on a riser is the upgrade |")
    A("| 15 | FE model bonds every joint and fixes the whole base underside | F | structure column is a lower bound | joints added by hand; see 1 and 2 |")
    A("| 16 | Home sensor sees a steel pin flush in an aluminium face at 1 mm | G | false triggers from chips | home with the coolant off; rcnc checks the pulse width |")
    A("| 17 | Splash ring and bearing seals are enough against low-pressure flood | G | bearing life | drain at the bottom; grease the bearing through its ports yearly |")
    A("| 18 | Motor price is a reseller's; StepperOnline's own page would not load. Plate prices are a shop's formula for small cut pieces | D / est | BOM total | check at order time |")
    A("| 19 | Prices and lead time: the reducer is listed as pre-order | D | schedule | ask the vendor |\n")

    A("## 13. Files\n")
    A("| Path | What |\n|---|---|")
    A("| `REPORT.md`, `bom.csv` | this report; every part with vendor, number, quantity, price, link |")
    A(f"| `iter{it}/a-axis.json` | the final rCAD document: one Part Studio per part, the assembly with every screw, the exploded view |")
    A(f"| `iter{it}/a-axis-assembly.step`, `step/` | assembly STEP and one STEP per custom part |")
    A(f"| `iter{it}/machine/on-machine.json` | the assembly on the real machine model |")
    A("| `renders/` | whole assembly, exploded, section, on the mill, each custom part |")
    A("| `iter1` to `iter%d` | every iteration: model, STEP, renders, `calc.md`, `fea/`, `dfm.json` |" % it)
    A("| `scripts/` | `build.py` (model), `fea.py` (FE runs), `calc.py` + `phys.py` (hand calculations), `machine_check.py` (interference on the mill), `dfm.py` (can it be milled), `extras.py` (section, exploded), `bom.py`, `report.py`. Rerun in that order |")
    A("| `cutsim/` | rCAD cutting simulation behind the load cases |")
    A("| `vendor/` | vendor STEP, datasheets and the vise drawings, each with `sources.txt` |\n")
    A("Vendor CAD used: reducer and motor from StepperOnline's file host, RU85 from THK's CAD (via a public repository, THK's terms attached). Drawn by hand from published dimensions because no CAD exists: the vise, the M8 sensor, the spot drill, O-rings and small hardware. Screws, nuts, washers and the dowel pin are rCAD's ISO parts.\n")
    A("rCAD itself got one small change, on the same branch as its own commit: `solve_simulation` takes `probes`, points at which to report the displacement.\n")
    open(os.path.join(ROOT, "REPORT.md"), "w").write("\n".join(R))
    print("REPORT.md", sum(len(r) for r in R), "chars")


if __name__ == "__main__":
    main()
