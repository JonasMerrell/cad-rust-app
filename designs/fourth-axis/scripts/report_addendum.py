#!/usr/bin/env python3
"""Appends sections 14 and 15 to REPORT.md (after report.py 5 has written sections 1 to 13): iteration 6 and the cycloidal design.

    report.py 5 && report_addendum.py
"""
import os, json, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build6 import ROOT

c6 = json.load(open(os.path.join(ROOT, "iter6", "calc.json"))); m6 = json.load(open(os.path.join(ROOT, "iter6", "machine", "results.json")))
d6 = json.load(open(os.path.join(ROOT, "iter6", "dfm.json")))
k = c6["tool_point"]; must = [q for q in m6["poses"] if q["kind"] == "must be clear"]
real = [q for q in must if any(not (h["a"].startswith("base") and h["b"].startswith("table")) for h in q["hits"])]
fl = [q for q in m6["poses"] if q["kind"] == "front-left case"]
t = c6["torsion"]["table"]
L = ["", "## 14. Iteration 6: the low headstock that leaves the tool setter working (added 2026-11-20)\n",
     "Asked for after iteration 5: *the tool setter needs to still work; steel parts are allowed if that makes it smaller.* Files in `iter6/`, built by `scripts/build6.py`.\n",
     "- **Turned end for end**: headstock and motor at the right end of the table, work and tailstock toward the left, where the tool setter stands (its centre on the back-left table corner).",
     "- **RU66 main bearing** (35 x 95 x 15) instead of RU85, round-topped plates of R56: centre height 63.9 mm instead of 77.2, the whole unit 120 mm tall. A retracted 24 mm tool's tip is 126 mm above the table, so the head passes over everything.",
     "- **Steel (1018 cold-rolled flat)** for the faceplate (9.5 mm, Ø100), the spindle nose, two identical hub discs and the base plate (7.9 mm). The steel faceplate is also the home sensor's target: a flat on its rim.",
     "- Spindle stack clamped from the front by four M6 through the bearing's bore; brake piston in its own plate in front of the reducer.",
     f"- **Machinability**: {sum(1 for v in d6.values() if v['ok'] and v['fits_travel'])} of {len(d6)} machined parts pass the one-cutter check.",
     f"- **Stiffness**: {k['chuck_Fz']['total_less_bar']*391e6:.1f} µm at the jaws under the mean roughing force, {k['chuck_Fz']['total_less_bar']*78e6:.1f} µm finishing (targets 20 and 5); "
     f"{k['overhang_Fz']['total_less_bar']*391e6:.0f} µm at 100 mm overhang without the tailstock. The smaller bearing costs stiffness against iteration 5 ({c6['bearing']['k_tilt']:.2e} N·m/rad of tilt).",
     f"- **Windup** as iteration 5: {t[0]['um']:.1f} µm at 46 mm on the gear alone at the mean roughing torque, {t[0]['um_brake']:.1f} µm with the brake.",
     f"- **On the machine** (`scripts/machine_check6.py`, {len(m6['poses'])} poses): {len(must) - len(real) - 1} of {len(must)} required poses clear, among them touching the tool setter with the 21 mm tool, the 24 mm tool and the 40 mm probe, "
     "and the route to it along the back edge with Z fully up. The one flagged required pose is the machine model's own base against its own table at the end of Y travel, which is there without the A axis. "
     f"The front-left corner case ({len(fl)} poses, outside the CAD's Y travel, geometry only): {sum(1 for q in fl if not q['hits'])} clear. "
     "The 40 mm probe cannot pass over the headstock on the axis line (its tip is 110 mm above the table); it passes beside it or over the work.",
     "- The motor still hangs past the right end of the table (to X 285 in the model, tub wall at 294.8). The cycloidal design below removes that.",
     "- Not done for iteration 6: its own modal run and a new BOM (it differs from iteration 5's by the RU66, the steel stock and the O-ring sizes).\n",
     "### Two corrections to earlier numbers\n",
     "1. **Structure FE runs.** They named no material, and rCAD then left one part out of the mesh. With the material named, the structure's share of the deflection at the jaws is 2.1 µm, not 6.4 (iteration 5) or 4.4 (iteration 6). "
     "Sections 1 and 7 above are regenerated with the corrected runs: iteration 5 is 9.5 µm at the jaws (13.6 reported before), iteration 6 is 14.4 (16.7 reported before). The error was on the safe side. "
     "Iteration 5's modal run and the runs for iterations 1 to 4 were not repeated.",
     "2. **Machine check of the turned unit.** The first run of `machine_check6.py` applied the half turn again at every pose, so every second pose had the unit facing the wrong way. Fixed; the figures above are from the corrected run.\n",
     "## 15. The cycloidal design (now the main one)\n",
     "Jonas then asked for a self-made cycloidal reducer as the main design, held to tighter tolerances by careful work and in-situ probing. It keeps iteration 6's front end and replaces the bought reducer and the NEMA 34: "
     "see **`cycloidal/REPORT.md`** (result, tolerance study, four iterations c1 to c4, the comparison of the three drives, the upgrade path back to the strain-wave unit) and **`cycloidal/MAKING.md`** (making plan, probe-and-correct, calibration, rcnc requirements).\n"]
p = os.path.join(ROOT, "REPORT.md"); s = open(p).read()
if "## 14. Iteration 6" in s: s = s[:s.index("## 14. Iteration 6")].rstrip() + "\n"
open(p, "w").write(s.rstrip() + "\n" + "\n".join(L))
print("appended", len(L))
