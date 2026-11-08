# Simulating the 6-axis arm

A rigid-body simulation of `../parts.rhai` with 1 kg on the tool: a pick-and-place cycle timed as fast as the
steppers allow, each motor's torque against its pull-out curve at the speed it turns, the tool changer checked
for lift-off, collisions on the CAD meshes, joint limits, payload capacity and the reachable space.

    mkdir sim-meshes && rcad script ../parts.rhai sim-meshes sim     # every part as an STL + sim-parts.json (~2 min)
    pip install -r requirements.txt
    SIMOUT=sim-meshes python3 model.py       # masses and inertias per link -> arm.xml, model.pkl
    python3 tasks.py                         # everything below -> out/results.json (~1.5 min)
    SIMOUT=sim-meshes python3 export_web.py  # light meshes + results for a 3D viewer -> out/viewer-data.json

| File | What it does |
|---|---|
| `model.py` | Groups the 90 parts into base, J1…J6 and tool, scales each part's mesh to the mass rcad gives it and sums mass, centre of mass and inertia per link. Writes a MuJoCo model whose body frames all sit at the world origin in the zero pose, with each joint on the axis `parts.rhai` uses. The payload is 1 kg at rcad's tool centre of gravity. |
| `arm.py` | Forward and inverse kinematics, inverse dynamics (MuJoCo), the wrench on the tool interface, and the actuator model: a dq model of each stepper at 24 V with the TMC2209 capped at 1.8 A RMS, ratio, 75 % gearbox efficiency (95 % more for the J2 belt), rotor inertia reflected through the ratio, and a 70 % cap on pull-out torque. |
| `collide.py` | Exact triangle collision tests (FCL) between links and against the table top. Neighbouring links are checked with the gearbox between them left out. |
| `tasks.py` | The studies, below. |

## Results

| | |
|---|---|
| Static check | Matches rcad's `summary.json` to 0.3 % (J2 14.36 against 14.40 N·m stretched out). |
| Payload | Holds 1 kg in every pose using at most 59 % of the usable torque. The heaviest tool it can hold still is 2.2 kg (J3, stretched out); J5 caps it at 2.5 kg elsewhere. |
| Cycle | Fastest stepper-safe pick-and-place: 5.9 s, tool up to 1.4 m/s. Six of the seven moves are limited by J2, whose 100:1 reduction runs its motor at 1430 rpm. 25 % faster stalls J1 and J2. |
| Tool changer | Two M4 lock screws at 0.5 N·m (about 625 N each) clamp the coupling: a ball would lift off only at 25× the load held level at the worst roll, 30× through the cycle, 18× at 1.5× speed. The magnets alone gave 1.15. At 0.5 N·m each ball presses 242 N into its Ø4 dowels, a Hertz pressure of 4.7 GPa, above the ~4.2 GPa where hardened steel dents; 0.2 N·m gives 3.6 GPa and still a factor of 11. |
| Collisions | None along the cycle. 15 % of random poses inside the limits collide, mostly the forearm or wrist folding back into the turntable or base. |
| Joint limits | J2 down to 31° below level (the shoulder hub's tail meets the turntable at 34°; it was 40° before the turntable went to 20 mm), J3 −145…146° (self-collision at −148/+149°), J5 −110…118° at any J4. J1, J4, J6 ±170° for cables. |
| Reach | Tool face centre 512 mm from the J1 axis, 721 mm above the table. |

The ball contact normals sit 26° from the coupling axis.

Not modelled: gearbox backlash and stiffness, link deflection (the FEA in rcad can do that), cable drag,
friction beyond the efficiency figure. Motor resistance, inductance and rotor inertia are typical datasheet
values, not measurements.
