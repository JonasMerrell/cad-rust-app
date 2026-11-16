# G-code simulation on the printer's own CAD

`simulate_gcode` runs a 3D printer's G-code on the printer's assembly in rCAD. You can use it to check a change to the machine (a new part, a moved panel, a longer extrusion) or to the G-code (a new start macro, a toolchange path, non-planar slicing) before the real printer runs it.

```json
{"tool": "open_document", "args": {"path": "X-carriage.json"}},
{"tool": "simulate_gcode", "args": {
  "assembly": "Printer",
  "gcode": "benchy.gcode",
  "config": "simplecore-config/config/printer.cfg",
  "directions": {"x": [0, -1, 0], "y": [1, 0, 0], "z": [0, 0, 1]},
  "near_miss": 1,
  "renders": 6, "render_dir": "sim", "report": "sim/report.json"
}}
```

## What it runs

The G-code goes through Klipper's own config, so it runs what the printer would run, not only what the slicer wrote. That covers:

- **printer.cfg and its includes**, including the SAVE_CONFIG block.
- **`[gcode_macro]`s**, through a Jinja interpreter:
  - `{% set %}`, `{% if %}` and `{% for %}`
  - `params`, filters, `%` formatting
  - `printer.toolhead`, `printer.gcode_move` and `printer["gcode_macro X"]` with their live values
  - `SET_GCODE_VARIABLE`, `SAVE_GCODE_STATE` and `RESTORE_GCODE_STATE`
  - `rename_existing`
  - `action_raise_error`, which stops the run as Klipper would
- **Homing**:
  - XY to the endstops
  - Z on a switch, `safe_z_home`, or a probe's `home_xy_position` (Beacon, Cartographer, probe)
  - `METHOD=CONTACT` homing
  - `homing_override`
- **Levelling and the bed mesh**:
  - `Z_TILT_ADJUST` and `QUAD_GANTRY_LEVEL` at their points
  - `BED_MESH_CALIBRATE` with the probe's offset applied. Scanning probes sweep their rows, contact probes dip at each point.
- **Trad Rack toolchanges**: `T<n>` and `TR_LOAD_TOOLHEAD` run `pre_unload_gcode` and `post_load_gcode`, so parking and the cutter moves are simulated.
- **Moves and coordinates**:
  - `G0`, `G1`, `G2` and `G3` (arcs at `[gcode_arcs] resolution`), and `G4`
  - `G90`/`G91`, `M82`/`M83` and `G92`
  - `SET_GCODE_OFFSET` (with `MOVE=1`), `M220`/`M221` and `SET_KINEMATIC_POSITION`
- **Klipper's own errors**:
  - *Move out of range* against `position_min` and `position_max`
  - moves before homing
  - unknown commands, which are listed and assumed not to move
- **Time**: Klipper's look-ahead planner gives the print time, using `max_accel`, `square_corner_velocity` and the Z limits.

## How the CAD follows the G-code

The assembly's three slider mates are the axes.

- **Which mate is which axis.** The simulator moves each mate a little and sees how the nozzle moves relative to the bed. Each Klipper axis gets the mate that moves the nozzle along its `directions` entry, which defaults to the world axes. CoreXY or cartesian makes no difference: the model's sliders are cartesian. If there aren't exactly three sliders, name them with `axes`.
- **Where 0 is.**
  - X and Y home to `position_endstop` against the end of their mate's travel. `endstop_clearance` shortens that, and `origin` overrides it.
  - Z 0 is where the nozzle tip touches the bed under it. The tip is the lowest point of a toolhead part called "nozzle" (or whatever `nozzle_part` names). With no such part it is the toolhead's lowest point, and the report says so. When that lowest point is a probe, the report says how far below the next part it hangs. Give `nozzle_part` or `nozzle` to fix the tip.
- **What isn't solid.** A Beacon or Cartographer model carries its sensing volume as round solids (a cylinder or cone about a vertical axis). These are left out of the checks and of the nozzle search (`ignore_sensing: false` keeps them). `ignore_parts` leaves out other parts by name, or one body of a part as `"Name#k"`. The report's notes list what was left out.
- **What moves.** Each instance moves with the mates as the solver moves it, as rigid groups such as the gantry, the toolhead, the bed and the still frame. Parts that turn, such as motors on belt relations, are held still.

## What it checks

Every move is checked over its whole length, not at sample points. Each group only translates, so a triangle swept along a move is the convex hull of its two end positions, and a distance query (GJK) on that hull bounds the whole move. Pairs that come within reach are then measured exactly, triangle to triangle, at the closest point along the move. A thin wall can't be skipped between samples.

| event | meaning |
|---|---|
| `collision` | Two parts that move relative to each other touch (within `clearance`, 0.05 mm by default). For example, the toolhead hits the frame, the gantry hits a panel, or the bed hits the toolhead. |
| `print_collision` | The toolhead, gantry or anything else hits the part being printed. |
| `travel_limit` | A mate is driven past its limits (by more than `clearance`). |
| `near_miss` | Parts come closer than `near_miss` mm. This is off unless you set `near_miss`. |
| `nozzle_on_bed` | The nozzle touches the bed at Z 0 during contact probing. This is expected and isn't counted as a problem. |

Each pair of parts is one event, however many moves bring them together. The event gives how many moves, the first lines, and the closest point.

Some parts touch wherever the axes are, such as rails and their carriages, or wheels and their tracks. The simulator finds these pairs by checking the model's own pose and a second pose with every axis moved, and ignores them. They are listed under `mapping.riding_pairs_ignored`.

### The printed part

Each extruding move leaves a bead along its path. The bead is the slicer's `;WIDTH:` wide and `;HEIGHT:` tall. When the slicer doesn't give those, the size is worked out from the filament pushed. The bead's top sits `print_allowance` (0.1 mm by default) under the nozzle that laid it, so the nozzle riding on its own layer isn't a hit.

The beads are kept in columns that know their highest bead. A moving part is only checked against beads that stand higher than its lowest point. Ordinary planar layers lie under the nozzle and cost nothing to check. Non-planar moves, travels through finished walls and a gantry sweeping over a tall print are found exactly.

## Output

- `summary`:
  - `verdict` is `clear` or `problems`
  - counts by kind
  - print time
  - moves and beads
  - the run time
- `events`, each with:
  - the G-code line and its text, and the macros it ran through (`via`, for example `START_PRINT > T0 pre_unload_gcode > SHAPE_TIP`)
  - the two parts
  - the closest distance, or how far past a limit
  - where it happened
  - the toolhead position and the time
- `klipper_issues`: what Klipper itself would complain about.
- `mapping`: how the axes, origin, groups and nozzle were found. Check this the first time you run a new model.
- `renders`: PNG close-ups of the first events, with the two parts in red and orange and the print in green.
