"""Check the feature-based project (project.rhai) against parts.rhai, part by part:

    python3 examples/fpv-tank/check_project.py <rcad binary> <work folder>

Builds both, then samples each part on a grid and prints the volume found in only one of them (it
should be no more than the grid's sampling noise at curved faces, a few mm³).
"""
import json, os, subprocess, sys

here = os.path.dirname(os.path.abspath(__file__))
rcad, work = sys.argv[1], os.path.abspath(sys.argv[2])
ref, nat = os.path.join(work, "reference"), os.path.join(work, "project")
os.makedirs(ref, exist_ok=True); os.makedirs(nat, exist_ok=True)

def run(*args):
    r = subprocess.run([rcad, "script", *args], capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(r.stdout[-3000:] + r.stderr[-3000:])
    return json.loads(r.stdout.strip().splitlines()[-1])

run(os.path.join(here, "parts.rhai"), ref, "reference")
run(os.path.join(here, "project.rhai"), nat)
X, BZ = 24.0 + 11.0, 65.5 - 8.0          # barrel's back end in the tank
CUP = [-14.0, 12.0, 43.4 + 4.0 + 34.0]   # cupola's underside centre
checks = [  # (Part Studio, reference, grid pitch, offset, solid)
    ("Road wheel", "road-wheel", 0.3, None, None), ("Idler", "idler", 0.3, None, None), ("Sprocket", "sprocket", 0.3, None, None),
    ("Barrel", "barrel", 0.3, [X, 0, BZ], None), ("Cupola", "cupola", 0.2, CUP, None), ("Turret ring", "turret-ring", 0.4, None, None),
    ("Gun drum", "gun-drum-left", 0.4, None, "left"), ("Gun drum", "gun-drum-right", 0.4, None, "right"), ("Turret shell", "turret-shell", 0.6, None, None),
    ("Deck", "deck", 0.7, None, None), ("Track", "track", 0.7, None, None), ("Hull", "hull-tub", 0.8, None, None)]
for studio, r, h, off, solid in checks:
    args = [os.path.join(here, "check_project.rhai"), os.path.join(nat, studio + ".step"), os.path.join(ref, r + ".step"), str(h)]
    if off or solid is not None:
        args += [str(v) for v in (off or [0, 0, 0])]
    if solid is not None:
        args.append(str(solid))
    res = run(*args)
    print(f"{r:16s} native {res['native_mm3']:9.1f}  reference {res['reference_mm3']:9.1f}  only native {res['only_native_mm3']:6.1f}"
          f"  only reference {res['only_reference_mm3']:6.1f} mm³ (grid {h})", flush=True)
