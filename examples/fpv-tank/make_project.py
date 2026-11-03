"""Build an rCAD project ZIP of the FPV tank from project.rhai: one Part Studio per printed part with
its sketches, dimensions and features, the bought parts as stand-ins, and an assembly with mates.

    python3 examples/fpv-tank/make_project.py <rcad binary> <output folder>

writes <output folder>/FPV Tank.zip, which rCAD opens with File > Import project (ZIP)….
Needs an rCAD with the script document API and d.tool (#102, #103).
"""
import json, os, shutil, subprocess, sys, zipfile

here = os.path.dirname(os.path.abspath(__file__))
rcad, out = sys.argv[1], os.path.abspath(sys.argv[2])
NAME = "FPV Tank"
work = os.path.join(out, "build")
proj = os.path.join(out, NAME)
stand = os.path.join(work, "bought")
shutil.rmtree(work, ignore_errors=True); shutil.rmtree(proj, ignore_errors=True)
os.makedirs(stand); os.makedirs(proj)

def run(*args):
    r = subprocess.run([rcad, "script", *args], capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(r.stdout[-3000:] + r.stderr[-3000:])
    return json.loads(r.stdout.strip().splitlines()[-1])

# 1. the bought parts (and everything else) where they sit in the tank, one STEP each
parts = run(os.path.join(here, "parts.rhai"), stand, "project")
json.dump(parts, open(os.path.join(stand, "list.json"), "w"))

# 2. the feature-based project: Part Studios, stand-ins, assembly
studios = run(os.path.join(here, "project.rhai"), work, "", stand)
doc = json.load(open(os.path.join(work, NAME + ".json")))
# the document's own first Part Studio holds the first part, the road wheel (scripts can't name it)
doc["studios"][0]["name"] = "Road wheel"
json.dump(doc, open(os.path.join(proj, NAME + ".json"), "w"), indent=1)

# 3. what goes with it: the scripts, the README and the print files
os.makedirs(os.path.join(proj, "scripts")); os.makedirs(os.path.join(proj, "print"))
for f in ["project.rhai", "feat.rhai", "parts.rhai", "anim.rhai", "motion.rhai", "check_overhangs.py", "make_project.py"]:
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
print(z, len(studios), "Part Studios")
