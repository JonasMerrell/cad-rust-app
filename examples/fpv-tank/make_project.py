"""Build an rCAD project ZIP of the FPV tank: a Part Studio with every part where it sits in the
tank (named, coloured, with its material), plus the scripts, README and print STLs.

    python3 examples/fpv-tank/make_project.py <rcad binary> <output folder>

writes <output folder>/FPV Tank.zip, which rCAD opens with File > Import project.
"""
import json, os, shutil, subprocess, sys, zipfile

here = os.path.dirname(os.path.abspath(__file__))
rcad, out = sys.argv[1], os.path.abspath(sys.argv[2])
NAME = "FPV Tank"
work = os.path.join(out, "build")
proj = os.path.join(out, NAME)
shutil.rmtree(work, ignore_errors=True); shutil.rmtree(proj, ignore_errors=True)
os.makedirs(work); os.makedirs(proj)

# 1. every part in place, one STEP each
r = subprocess.run([rcad, "script", os.path.join(here, "parts.rhai"), work, "project"], capture_output=True, text=True, check=True)
parts = json.loads(r.stdout.strip().splitlines()[-1])

PRINTED = {"hull-tub": "PLA", "deck": "PLA", "turret-ring": "PLA", "turret-shell": "PLA", "cupola": "PLA",
           "gun-drum-left": "PLA", "gun-drum-right": "PLA", "barrel": "PLA", "sprocket": "PLA", "idler": "PLA",
           "road wheel": "PLA", "pushrod": "Steel"}

def material(name):
    if name.startswith("track"):
        return None   # TPU isn't in rCAD's material list
    for k, m in PRINTED.items():
        if name.startswith(k):
            return m
    return None

# 2. a tool script that imports each part (rCAD stores the STEP in the document)
steps = [{"tool": "new_document", "args": {"units": "mm"}}]
for p in parts:
    steps.append({"tool": "import_file", "args": {"path": os.path.join(work, p["file"]), "name": p["name"]}})
steps.append({"tool": "save_document", "args": {"path": os.path.join(work, "imports.json")}})
tools = os.path.join(work, "tools.json")
json.dump(steps, open(tools, "w"))
r = subprocess.run([rcad, "run", "--stop-on-error", tools], capture_output=True, text=True)
if r.returncode != 0:
    sys.exit(r.stdout[-3000:] + r.stderr[-3000:])
doc = json.load(open(os.path.join(work, "imports.json")))
studio = doc["studios"][0]
studio["name"] = NAME
fids = [f["id"] for f in studio["doc"]["features"] if f["op"].get("type") == "import"]   # in import order
assert len(fids) == len(parts)

# 3. name, colour and material of each part (per-part settings are keyed "feature:body")
props = {}
count = {}
total = {}
for p in parts: total[p["name"]] = total.get(p["name"], 0) + 1
feats = {f["id"]: f for f in studio["doc"]["features"]}
for fid, p in zip(fids, parts):
    # number parts that come more than once (road wheels)
    if total[p["name"]] > 1:
        count[p["name"]] = count.get(p["name"], 0) + 1
        p["name"] = f"{p['name']} {count[p['name']]}"
        feats[fid]["name"] = p["name"]
    c = p["color"].lstrip("#")
    e = {"name": p["name"], "color": [int(c[k:k + 2], 16) for k in (0, 2, 4)], "material": material(p["name"])}
    props[f"{fid}:0"] = e
studio["doc"]["parts"] = props
json.dump(doc, open(os.path.join(proj, NAME + ".json"), "w"), indent=1)

# 4. what goes with it: the scripts, the README and the print files
os.makedirs(os.path.join(proj, "scripts")); os.makedirs(os.path.join(proj, "print"))
for f in ["parts.rhai", "anim.rhai", "motion.rhai", "check_overhangs.py", "make_project.py"]:
    shutil.copy(os.path.join(here, f), os.path.join(proj, "scripts", f))
shutil.copy(os.path.join(here, "README.md"), os.path.join(proj, "README.md"))
for f in sorted(os.listdir(os.path.join(here, "stl"))):
    shutil.copy(os.path.join(here, "stl", f), os.path.join(proj, "print", f))

z = os.path.join(out, NAME + ".zip")
with zipfile.ZipFile(z, "w", zipfile.ZIP_DEFLATED) as zf:
    for root, _, files in os.walk(proj):
        for f in sorted(files):
            full = os.path.join(root, f)
            zf.write(full, os.path.relpath(full, out))
shutil.rmtree(work)
print(z, len(parts), "parts")
