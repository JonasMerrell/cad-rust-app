#!/usr/bin/env python3
"""bom.csv for the final iteration: counts come from the rCAD assembly, vendor / part number / price / link from the table below.

    bom.py <iteration>      -> bom.csv in the project folder

Price source tags: page = read on the vendor's page 2026-11-20; snippet = from a search result only; est = estimate.
"""
import sys, os, json, csv
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build import ROOT

SO = "https://www.omc-stepperonline.com/"
BOUGHT = [  # name, vendor, part number, qty, unit price USD, price source, link, note
    ("Strain wave reducer, size 25, 100:1, crossed-roller output", "StepperOnline", "HSS-25-100-I-D14", 1, 268.84, "page (pre-order)",
     SO + "hss-series-harmonic-drive-gearbox-107mm-ratio-100-1-backlash-20arcsec-14mm-input-diameter-hss-25-100-i-d14", "vendor STEP in the model"),
    ("Closed-loop stepper NEMA 34, IP65, 4.5 N·m, 14 mm keyed shaft, 1000-line encoder", "StepperOnline (price: Oyostepper)", "34E1KIP65-45", 1, 125.70, "page (reseller)",
     SO + "p-series-ip65-waterproof-nema-34-closed-loop-stepper-motor-4-5nm-637-38oz-in-with-encoder-1000ppr-4000cpr-34e1kip65-45", "vendor STEP in the model"),
    ("Closed-loop stepper drive, 24-110 VDC, step/dir to 200 kHz", "StepperOnline", "CL86T-V41", 1, 46.31, "page",
     SO + "closed-loop-stepper-driver-v4-1-0-8-2a-18-80vac-24-110vdc-for-nema-34-stepper-motor-cl86t-v41", "not in the model (lives in the control cabinet)"),
    ("48 V 350 W power supply", "Mean Well (Jameco)", "LRS-350-48", 1, 33.30, "snippet",
     "https://www.jameco.com/z/LRS-350-48-MEAN-WELL-350W-48V-7-3A-Single-Output-Enclosed-Switching-Power-Supply_2219840.html", "not in the model"),
    ("Crossed roller bearing 55 x 120 x 15, both rings with mounting holes, P5", "THK type (eBay LKPB store)", "RU85UUCC0 P5", 1, 179.00, "snippet",
     "https://www.ebay.de/itm/134210207312", "THK CAD in the model; genuine THK is about 700-820 USD"),
    ("Self-centring vise 75 x 54 x 44, 8-55 mm", "XinDian (PAF)", "H54-8-55", 1, 0.0, "owned / chosen by Jonas", "https://www.amazon.com/dp/B0CKWBBHGS",
     "no vendor CAD exists: drawn from the vendor's two dimension drawings"),
    ("O-ring NBR 70, 92 x 2.5", "The O-Ring Store", "N2.50X092", 1, 0.45, "page", "https://www.theoringstore.com/store/index.php?main_page=advanced_search_result&keyword=2.50X092", "brake piston, inner"),
    ("O-ring NBR 70, 118 x 2.5", "The O-Ring Store", "N2.50X118", 1, 0.58, "page", "https://www.theoringstore.com/store/index.php?main_page=advanced_search_result&keyword=2.50X118", "brake piston, outer"),
    ("3/2 solenoid valve, 1/8 in ports, 24 V DC, normally closed", "Airtac type (STC Valve)", "3V110-06-NC 24VDC", 1, 40.76, "snippet", "https://www.stcvalve.com/Solenoid_Valve.htm", "brake; not in the model"),
    ("Push-in union 6 mm", "Aignep (Zoro), pack of 5", "50040N-6", 1, 28.15, "snippet", "https://www.zoro.com/aignep-usa-tube-union-6mm-tube-sz-brass-pk5-50040n-6-pk5/i/G0252147/", "tube stub to hose; not in the model"),
    ("Polyurethane tube 6 x 4 mm, 20 m", "SMC (Motion)", "TU0604", 1, 19.19, "snippet", "https://www.motion.com/products/sku/02005429", "not in the model"),
    ("Brass tube 6 mm OD x 300 mm, 2 pieces (cut one 60 mm stub)", "K&S", "9824", 1, 10.99, "snippet", "https://ksmetals.com/products/br45mm-6", "air port, bonded into plate C"),
    ("Inductive sensor M8, NPN NO, 2 mm, 6-36 V", "generic (Abra Electronics)", "LJ8A3-2-Z/BX", 1, 9.96, "snippet", "https://abra-electronics.com/", "home sensor; drawn from listing dimensions"),
    ("Spotting drill 12 mm, 90 degrees, solid carbide, 65 mm long", "Garr Tool (Zoro)", "91568", 1, 67.89, "snippet",
     "https://www.zoro.com/garr-tool-nc-spotting-drill-general-purpose-series-1600-12-mm-diameter-65-mm-overall-length-91568/i/G216069782/", "tailstock centre (used as a dead centre); check the point is 90 degrees"),
    ("Shim washers 8 x 14 or 16 mm, assorted 0.1 / 0.2 / 0.5 mm", "generic (DIN 988)", "DIN 988 8x14 assortment", 1, 12.00, "est", "", "6 stacks between plates C and D, thickness chosen at assembly"),
    ("Heat-set insert M5 x 7", "generic", "M5 x 7 brass", 2, 0.20, "est", "", "sensor bracket"),
    ("Loctite 638 retaining compound, 10 ml", "Henkel (Hisco)", "1835937", 1, 20.44, "snippet", "https://www.hisco.com/Product/1835937-30769", "wave generator key, home target pin"),
    ("RTV silicone gasket maker, 3.5 oz", "Permatex (Zoro)", "82194", 1, 9.55, "snippet", "https://zoro.com/permatex-rtv-silicone-sealant-35-oz-tube-gray-82194/i/G0600415", "plate joints, splash ring"),
    ("Wet-and-dry silicon carbide paper P400, 5 sheets", "3M (Zoro)", "32038", 1, 7.49, "snippet", "https://www.zoro.com/3m-3m-wetordry-sandpaper-32038-400-grit-9-inch-x-11-inch-5pk-7010328058/i/G712337491/", "friction lining, bonded with epoxy"),
    ("Slow epoxy, small pack", "generic", "", 1, 8.00, "est", "", "lining and air tube"),
    ("Strain wave gear grease", "Harmonic Drive", "SK-1A (or the grease StepperOnline specifies)", 1, 40.00, "est", "https://www.harmonicdrive.net/products/harmonicgrease", "the unit ships greased; top up per the vendor manual"),
]
STOCK = [  # 6061 plate pieces (Midwest Steel Supply calculator, computed by its formula; extruded flat bar is far cheaper where it fits)
    ("6061-T6 plate 1/2 in x 8 x 9 in", "base plate (212 x 156) is larger: order 1/2 x 7 x 9 in", "Base plate, Faceplate (second piece 1/2 x 5 x 5)", 2, 115.82, "est"),
    ("6061-T6 plate 5/8 in x 5 x 5 in", "", "Spindle nose", 1, 101.42, "est"),
    ("6061-T6 plate 3/4 in x 5 x 5 in", "", "Tailstock base", 1, 118.86, "est"),
    ("6061-T6 plate 1 in x 6 x 12 in", "", "Rotor, Tailstock plate", 1, 150.00, "est"),
    ("6061-T6 plate 1-1/4 in x 7 x 12 in", "", "P1 bearing plate, D motor plate", 1, 200.33, "est"),
    ("6061-T6 plate 1-1/2 in x 7 x 6 in", "", "C cylinder plate", 1, 126.67, "est"),
    ("6061-T6 plate 1/4 in x 6 x 6 in", "", "Brake piston", 1, 65.70, "est"),
]
SCREW_PRICE = {"M5": 0.15, "M6": 0.20, "M8": 0.35, "M8x120": 1.20}


def main(it):
    out = os.path.join(ROOT, f"iter{it}")
    ai = json.load(open(os.path.join(out, "assembly_info.json"))); meta = json.load(open(os.path.join(out, "meta.json")))
    kinds = {q["name"]: q for q in meta["parts"]}
    rows = [["item", "description", "kind", "vendor", "part number", "qty", "unit price USD", "price source", "link", "material / stock / note"]]
    n = 0; total = 0.0
    def add(desc, kind, vendor, pn, qty, price, src, link, note):
        nonlocal n, total
        n += 1; rows.append([n, desc, kind, vendor, pn, qty, f"{price:.2f}" if price is not None else "", src, link, note])
        if price: total += price*qty
    counts = {}
    for b in ai["bom"]:
        nm = b["part"].split(" / Part")[0]; counts[nm] = counts.get(nm, 0) + b["qty"]
    for nm, q in counts.items():
        if nm in kinds and kinds[nm]["kind"] == "machined":
            add(nm, "machined on the DMC2", "made", "", q, None, "", "", f"{kinds[nm]['material']}; stock: {kinds[nm]['stock']}")
    for nm, q in counts.items():
        if nm in kinds and kinds[nm]["kind"] == "printed":
            add(nm, "3D printed, no supports", "made", "", q, None, "", "", "ASA (coolant resistant)")
    for nm, q in counts.items():
        if nm in kinds and kinds[nm]["kind"] == "cut":
            add(nm, "cut by hand", "made", "", q, None, "", "", kinds[nm]["stock"])
    for desc, vendor, pn, qty, price, src, link, note in BOUGHT: add(desc, "bought", vendor, pn, qty, price, src, link, note)
    for nm, q in sorted(counts.items()):
        if nm.startswith("ISO "):
            std, size = nm.split(" ", 2)[1], nm.split(" ", 2)[2]
            m = size.split("x")[0]
            price = SCREW_PRICE.get(size, SCREW_PRICE.get(m, 0.1)) if std == "4762" else (0.10 if std in ("4032", "4035") else 0.05 if std == "7089" else 0.30)
            what = {"4762": "Socket head cap screw, class 12.9", "4032": "Hex nut, class 8", "4035": "Thin hex nut", "7089": "Flat washer", "8734": "Dowel pin, hardened (home target)"}[std]
            add(f"{what} {size}", "bought", "any metric fastener shop (e.g. Monster Bolts)", f"ISO {std} {size}", q, price, "est (M6: page)", "https://monsterbolts.com/", "")
    for desc, note, parts, qty, price, src in STOCK:
        add(desc, "stock", "Midwest Steel Supply (or any 6061 supplier)", "", qty, price, src + ": the shop's formula, not a quote", "https://www.midweststeelsupply.com/store/6061aluminumplate", f"for: {parts}. {note}")
    rows.append(["", "TOTAL of the priced lines", "", "", "", "", f"{total:.2f}", "", "", "vise not counted; plate prices are for small cut pieces and fall a lot with bar stock or offcuts"])
    with open(os.path.join(ROOT, "bom.csv"), "w", newline="") as f: csv.writer(f).writerows(rows)
    print(len(rows) - 2, "lines, total", round(total, 2))
    return total


if __name__ == "__main__":
    main(int(sys.argv[1]))
