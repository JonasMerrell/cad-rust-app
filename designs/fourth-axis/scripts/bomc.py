#!/usr/bin/env python3
"""bom.csv for a cycloidal iteration: counts from the rCAD assembly, vendors and prices from the table below.

    bomc.py <n>      -> cycloidal/bom.csv

Price tags: page = read on the vendor's page 2026-11-20 (for the harmonic design); est = my estimate, not a quote
(the StepperOnline pages refused an automated read on 2026-11-20, so the motor and drive prices are estimates too).
"""
import sys, os, json, csv
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build6 import ROOT

SO = "https://www.omc-stepperonline.com/"
BOUGHT = [  # description, vendor, part number, qty, unit USD, source, link, note
    ("Closed-loop stepper NEMA 23, IP65, 2.0 N·m, 8 mm shaft, 1000-line encoder", "StepperOnline", "23E1KIP65-20", 1, 75.0, "est",
     SO + "p-series-ip65-waterproof-nema-23-closed-loop-stepper-motor-2nm-283-28oz-in-with-encoder-1000ppr-4000cpr-23e1kip65-20", "vendor STEP in the model"),
    ("Closed-loop stepper drive 24-48 VDC, step/dir", "StepperOnline", "CL57T-V41", 1, 35.0, "est",
     SO + "closed-loop-stepper-driver-v4-1-0-8-0a-24-48vdc-for-nema-17-23-24-stepper-motor-cl57t-v41", "in the control cabinet, not in the model"),
    ("36 V 350 W power supply", "Mean Well", "LRS-350-36", 1, 33.0, "est", "", "not in the model"),
    ("Crossed roller bearing 35 x 95 x 15, both rings with mounting holes, P5", "THK type (clone)", "RU66UUCC0 P5", 1, 120.0, "est", "", "drawn from THK's dimension table; genuine THK costs several times this"),
    ("Self-centring vise 75 x 54 x 44, 8-55 mm", "XinDian", "H54-8-55", 1, 0.0, "chosen by Jonas", "https://www.amazon.com/dp/B0CKWBBHGS", "drawn from the vendor's dimension drawings"),
    ("Dowel pin 8 m6 x 40, hardened (ISO 8734)", "any (buy 30; grade by micrometer, or pin gauges in 0.01 steps)", "ISO 8734 8x40", 24, 0.60, "est", "", "ring pins"),
    ("Needle bearing inner ring 7 x 10 x 10.5", "INA / any", "IR 7x10x10.5", 12, 3.00, "est", "", "output sleeves, two per post"),
    ("Deep groove ball bearing 20 x 37 x 9, sealed", "any (SKF, NSK, NTN)", "6904-2RS (61904-2RS1)", 2, 9.00, "est", "", "one per disc; normal clearance"),
    ("Deep groove ball bearing 12 x 28 x 8, sealed", "any", "6001-2RS", 2, 4.00, "est", "", "input shaft"),
    ("Precision shaft 12 h6 x 80, hardened", "any linear-shaft seller, cut to length", "W12 h6 x 80", 1, 8.00, "est", "", "input shaft; bushes bonded on with Loctite 638"),
    ("Steel tube 16 x 2, 100 mm", "any", "", 1, 4.00, "est", "", "spacer on the shaft, faced to length"),
    ("Shim rings 12 x 18, assorted 0.1 to 0.5", "generic (DIN 988)", "DIN 988 12x18", 1, 8.00, "est", "", "axial play of the shaft stack"),
    ("Timing pulley GT3 3 mm, 20 teeth, 8 mm bore, for 9 mm belt", "any (Gates PowerGrip GT3 profile)", "3MGT 20T", 1, 9.00, "est", "", "on the motor"),
    ("Timing pulley GT3 3 mm, 60 teeth, 12 mm bore, for 9 mm belt", "any", "3MGT 60T", 1, 16.00, "est", "", "on the input shaft"),
    ("Timing belt GT3 3 mm pitch, 345 mm (115 teeth), 9 mm wide", "Gates or equivalent", "345-3MGT-9", 1, 9.00, "est", "", "GT profile for its small tooth clearance, not HTD"),
    ("Shaft seal 60 x 75 x 8, NBR", "any", "60x75x8 TC", 1, 5.00, "est", "", "keeps grease out of the brake"),
    ("O-ring NBR 70, 68 x 2.5", "The O-Ring Store", "N2.50X068", 1, 0.45, "est", "", "brake piston, inner"),
    ("O-ring NBR 70, 100 x 2.5", "The O-Ring Store", "N2.50X100", 1, 0.55, "est", "", "brake piston, outer"),
    ("3/2 solenoid valve, 1/8 in ports, 24 V DC, normally closed", "Airtac type (STC Valve)", "3V110-06-NC 24VDC", 1, 40.76, "snippet", "https://www.stcvalve.com/Solenoid_Valve.htm", "brake; not in the model"),
    ("Push-in union 6 mm, tube 6 x 4, brass tube 6 mm", "as the harmonic design's BOM", "", 1, 58.33, "snippet", "", "air to the brake"),
    ("Inductive sensor M8, NPN NO, 2 mm", "generic", "LJ8A3-2-Z/BX", 1, 9.96, "snippet", "", "home sensor"),
    ("Spotting drill 12 mm, 90 degrees, solid carbide", "Garr Tool (Zoro)", "91568", 1, 67.89, "snippet", "", "tailstock centre"),
    ("Heat-set insert M5 x 7", "generic", "", 2, 0.20, "est", "", "sensor bracket"),
    ("Loctite 638, RTV gasket maker, P400 paper, epoxy", "as the harmonic design's BOM", "", 1, 45.48, "snippet", "", "bushes on the shaft, plate joints, brake lining"),
    ("Lithium EP2 grease, small tin", "any", "", 1, 8.00, "est", "", "reducer"),
    ("Fine lapping compound (or metal polish), small tin", "any", "", 1, 8.00, "est", "", "run-in, optional"),
]
STOCK = [
    ("O1 ground flat stock 1/4 x 4 x 18 in", "Cycloid discs A and B (cut stacked)", 1, 45.0),
    ("4140 pre-hardened or O1 flat 3/8 x 3-1/2 x 12 in", "Carrier 2 spider, eccentric bushes", 1, 40.0),
    ("1018 cold-rolled flat 1/2 x 6 x 12 in", "Pin plates G1 and G2 (bored stacked)", 1, 30.0),
    ("1018 cold-rolled flat 1/2 x 4 x 12 in", "Spindle nose, carrier 1, hub disc A", 1, 18.0),
    ("1018 cold-rolled flat 3/4 x 2-1/2 x 6 in", "Hub spacer", 1, 10.0),
    ("1018 cold-rolled flat 3/8 x 4 x 6 in", "Faceplate", 1, 9.0),
    ("1018 cold-rolled flat 5/16 x 6 x 12 in", "Base plate", 1, 22.0),
    ("6061-T6 plate 1-1/4 in x 7 x 6 in", "P1 bearing plate", 1, 100.0),
    ("6061-T6 plate 3/4 in x 7 x 12 in", "C1, M ring spacer, tailstock base", 1, 120.0),
    ("6061-T6 plate 5/8 in x 5 x 9 in", "D rear plate with the motor ear", 1, 60.0),
    ("6061-T6 plate 1 in x 5 x 4 in", "Tailstock plate", 1, 40.0),
    ("6061-T6 sheet 1/4 in and 1/8 in, 5 x 5 in each", "Brake piston, brake disc", 1, 35.0),
]
SCREW = {"M4": 0.10, "M5": 0.15, "M6": 0.20, "M8": 0.35, "M8x120": 1.20}


def main(n):
    out = os.path.join(ROOT, "cycloidal", f"c{n}")
    ai = json.load(open(os.path.join(out, "assembly_info.json"))); meta = json.load(open(os.path.join(out, "meta.json")))
    kinds = {q["name"]: q for q in meta["parts"]}
    rows = [["item", "description", "kind", "vendor", "part number", "qty", "unit price USD", "price source", "link", "material / stock / note"]]
    k = 0; total = 0.0
    def add(desc, kind, vendor, pn, qty, price, src, link, note):
        nonlocal k, total
        k += 1; rows.append([k, desc, kind, vendor, pn, qty, f"{price:.2f}" if price is not None else "", src, link, note])
        if price: total += price*qty
    counts = {}
    for b in ai["bom"]:
        nm = b["part"].split(" / Part")[0]; counts[nm] = counts.get(nm, 0) + b["qty"]
    for kind, label, note in (("machined", "machined on the DMC2", None), ("printed", "3D printed, no supports", "ASA"), ("cut", "cut by hand", None)):
        for nm, q in counts.items():
            if nm in kinds and kinds[nm]["kind"] == kind:
                add(nm, label, "made", "", 1, None, "", "", note or f"{kinds[nm]['material']}; stock: {kinds[nm]['stock']}")
    for row in BOUGHT: add(row[0], "bought", *row[1:])
    for nm, q in sorted(counts.items()):
        if nm.startswith("ISO "):
            std, size = nm.split(" ", 2)[1], nm.split(" ", 2)[2]; m = size.split("x")[0]
            price = SCREW.get(size, SCREW.get(m, 0.1)) if std in ("4762", "10642") else (0.10 if std in ("4032", "4035") else 0.05)
            what = {"4762": "Socket head cap screw, class 12.9", "10642": "Countersunk socket screw, class 10.9", "4032": "Hex nut, class 8", "4035": "Thin hex nut", "7089": "Flat washer"}.get(std, "ISO " + std)
            add(f"{what} {size}", "bought", "any metric fastener shop", f"ISO {std} {size}", q, price, "est", "", "")
    for desc, parts, qty, price in STOCK:
        add(desc, "stock", "any metal supplier (small cut pieces; offcuts are far cheaper)", "", qty, price, "est", "", "for: " + parts)
    rows.append(["", "TOTAL of the priced lines", "", "", "", "", f"{total:.2f}", "", "", "vise not counted; most prices are estimates"])
    with open(os.path.join(ROOT, "cycloidal", "bom.csv"), "w", newline="") as f: csv.writer(f).writerows(rows)
    print(len(rows) - 2, "lines, total", round(total, 2)); return total


if __name__ == "__main__":
    main(int(sys.argv[1]))
