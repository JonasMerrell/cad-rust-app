# Two-station part jobs (`plan_part_job`)

A small part (or a pair of parts on one blank) machined from both sides in a two-station vise, as one rCAD
job:

- **Op 1** cuts the raw blank in one station.
- **Op 2** cuts the part flipped, in the other station, from what op 1 left.
- One program, the **cycle**, runs op 2 on the previous blank and op 1 on the next.
- The **first** program (op 1 alone) starts a batch and the **last** (op 2 alone) ends it.

This is how the DMC2's gift parts (the ingot slider and its base, the Eclipse car and its thick-rail base)
are made. Before this tool, each job needed its own `cam.py`, `post.py`, `vise_machines.py` and six
G-code patch scripts.

## Once per machine: the vise stations

The machine model describes its vise once (`cam_machine`). The DMC2's two stations, from its vise CAD:

```json
{"tool": "cam_machine", "args": {"name": "DMC2",
  "move": {"Vise/Top Jaw": [0, -1.9, 0], "Vise/Gen3 Smooth Insert Bar 3": [0, -1.9, 0],
           "Vise/Top Jaw 2": [0, 1.9, 0], "Vise/Gen3 Smooth Insert Bar 4": [0, 1.9, 0]},
  "vise": [
    {"name": "left", "fixed_face": -7.8, "fixed_side": 1, "y": -350.3145, "ledge_z": 34.4348,
     "fixed_name": "fixed jaw (centre block)", "moving_face": -47.8,
     "moving": [{"part": "Vise/M12 Adjustable Base", "grid": 25}, {"part": "Vise/Top Jaw"}, {"part": "Vise/Gen3 Smooth Insert Bar 3"}]},
    {"name": "right", "fixed_face": 25.5, "fixed_side": -1, "y": -350.3145, "ledge_z": 34.4348,
     "fixed_name": "fixed jaw (centre block)", "moving_face": 143.3,
     "moving": [{"part": "Vise/M12 Adjustable Base 2", "grid": 25}, {"part": "Vise/Top Jaw 2"}, {"part": "Vise/Gen3 Smooth Insert Bar 4"}]}]}}
```

A setup then only names its station (`create_cam` `vise: {station}`). From the station, rCAD works out the
following:

- the jaws and any shims, as fixtures;
- the 0.3 mm clearance over the jaws;
- where the work sits on the table;
- rcnc's Find stock from the saved vise location (`VISE=`, `JAWAT=`);
- for the simulation, the model's moving jaw closed on the work.

## Each part: one call

Give the part's Part Studio, the blank **as measured**, its material and the carrier's two locating bores:

```json
{"tool": "plan_part_job", "args": {"name": "Base", "studio": 3,
  "stock": {"size": [26.6, 101, 12.34], "material": "6061-T6"},
  "carrier": {"points": [[-5, 0], [5, 0]]},
  "seam_chamfer": 1.7}}
```

It makes both setups with the rcnc post, the bog guard and stay-down links. The tools are the chamfer mill
(T1), the 1/4 in end mill (T24) and the probe (T99). It then adds the standard operations. Each operation
has a key, used to change or replace it.

**Op 1:**

| key | operation |
|---|---|
| `probe_top`, `probe_centre` | Find stock from the saved vise location |
| `face` | faces the blank's top, which is the part's bottom (`face`, default 0.2) |
| `rough` | adaptive roughing from the power budget, in climb laps round the parts down to just over the jaws |
| `walls` | the whole side profile in one full-height climb pass with arc leads |
| `carrier_bores` | the carrier's locating bores |
| `edge_break` | edge break |

**Op 2:**

| key | operation |
|---|---|
| `probe_top`, `probe_centre` | Find stock |
| `bore_a`, `bore_b` | a stop to blow the carrier bores out, then Find bores |
| `face` | faces down to the part's top |
| `band` | a light rough of the band above op 1's walls; it separates the parts |
| `seam_chamfer` | the top chamfer that hides the op 1 / op 2 seam |
| `edge_break` | edge break |

Speeds come from the material:
- **Aluminium:** roughing at 9000 rpm aimed at 650 W, capped at the spindle's caution; facing and finishing at 12000; the chamfer mill at 24000.
- **Brass:** mild, at 18000 rpm and 300 W.

The plan checks the arithmetic the job scripts did with asserts:
- how high op 1's walls reach in op 2;
- whether the seam chamfer's lower edge lies at least 0.2 under that height;
- whether the jaws hold enough of the blank;
- whether a Vise Z reference is used while the blank sits on shims.

The part's own operations are added as `extra` (pockets, channels, 3D finishing). Use `ops` to change a
standard one (for example a picked-edge deburr instead of a chamfer) and `skip` to leave one out. A third
setup in the job (an engraving) goes in `with` and is made after the call. All operations go in with
`build: false`, and both setups build once at the end. The result has the plan's numbers, its warnings,
both setups' times and warnings, and `steps`: the exact tool calls, which can be kept as the job's script.

Post the programs with `export_gcode`. Every program goes through the SIM OK gate. Batch posting in one call
is the next step.

## Acceptance: the posted gift jobs

The job scripts' `cam.py` (run with this build) and the template were compared with `compare_gcode`, for the
normal slider and base and for the Eclipse thick-rail base. Both were also compared with the programs posted
on rcnc `mill/ingot-slider-vise` fca5cd0.

| Job | template vs `cam.py` | template vs the posted programs |
|---|---|---|
| Slider (brass, on shims, with the engraving setup): op1, cycle, first, last | every operation identical | every operation identical; header identical |
| Base: op1, op2, cycle, first, last | every operation identical | identical but the carrier bores and magnet pockets (the 063b2ff bore lap fix) and the two deburrs (20ead45, arcs fitted only where the points lie on them): the base was posted before both fixes |
| Thick-rail base: op1, op2, cycle, first, last | every operation identical | identical but the bores (063b2ff): op 1 and op 2 were posted before that fix |

One control line differs in each op 2: the bores pause's message wording. It still names the bores, so
rcnc's probe tool change skips the stop once the bores were cleaned there. The guard line, the dwells and
the `VISE=`/`JAWAT=` requests come out as the patch scripts wrote them.
