# Using rCAD from an AI agent

rCAD has a headless interface with the same modelling kernel as the editor.
The program is `rcad` (`target/release/rcad` when built from source).
`rcad --help` lists the commands and `rcad --version` prints the version.
Agents can drive it in two ways:

- **MCP server** (`rcad mcp`): a Model Context Protocol server on stdio.
- **Script runner** (`rcad run script.json`): runs a JSON list of tool calls
  and prints one JSON result per line. This suits agents without MCP and CI.
  A failed step prints an `error` and makes the exit status 1; with
  `rcad run --stop-on-error script.json` the run stops at the first one.

Files saved with `save_document` open in the GUI (`rcad part.json`).

## Setup

Claude Code (user-wide):

    claude mcp add --scope user rcad -- rcad mcp

Or use the `.mcp.json` in the repository (and in the release archive, next to
the program), which Claude Code picks up when it runs in that folder; it starts
`rcad mcp`, so put the program on your PATH (or edit the command). Other MCP clients start the same command
(`rcad mcp`, transport stdio).

## Conventions

- Positions (sketch points, refs) are millimetres. Feature values such as
  `depth` are expressions in document units (mm by default) and may use
  variables, e.g. `"w / 2"`.
- Every change rebuilds the model. Results report each part's volume, area,
  bounding box and centroid, plus any feature errors.
- Geometry is referenced by refs: JSON objects such as
  `{"kind": "face", "point": [x, y, z], "normal": [nx, ny, nz]}` or
  `{"kind": "edge", "point": [x, y, z]}`. `list_geometry` returns ready-made
  refs for every face and edge; `sketch_info` returns refs for every region.
- `render` returns a PNG, so agents can check their work visually.
- Sketch points at the same position are merged automatically, so chains of
  items connect without extra constraints.
- Coplanar faces and collinear edges are merged after each operation, and
  fillet/chamfer errors name the edges that failed.
- Arguments are checked against each tool's schema (`tools/list`): an unknown
  argument, feature param, sketch item key or constraint key, a value outside
  a fixed set (views, formats, kinds), a sketch id that doesn't exist or a
  negative radius is an error that says what was meant, e.g.
  `extrude has no param 'dept'; did you mean 'depth'?`. `feature_defaults`
  gives any feature kind's params and their defaults.
- A call that leaves a feature failing (a fillet too big for its edge, say)
  keeps it, as the editor does, but its result has `"ok": false` and
  `new_errors`, MCP marks it `isError`, and `run` counts the step as failed.
  Pass `"rollback": true` to undo the whole call instead. A call that fails
  outright changes nothing.
- Output paths are relative to the working directory, and missing folders are
  created. Input files (`import_file`, `open_document`, images) given as
  relative paths are looked for in the working directory and then, for `run`,
  next to the script. Errors about files name the path.
- Results are compact JSON; `describe` shows an imported file's size and hash
  instead of its contents.

## Tools

| tool | what it does |
|---|---|
| `new_document` / `open_document` / `save_document` | manage the document (`.json`) |
| `describe` | features with full params, variables, parts, errors |
| `set_variable` | create or change a variable |
| `create_sketch` | sketch on `top`, `front`, `right` or a planar face ref |
| `sketch_add` | points, lines, polylines, rectangles, circles, arcs, ellipses, splines, polygons, text |
| `sketch_constrain` | geometric constraints and dimensions (values can be expressions) |
| `sketch_info` | points, curves, degrees of freedom, regions |
| `add_feature` / `edit_feature` / `delete_feature` | extrude, revolve, sweep, loft, fillet, chamfer, shell, hole, draft, thicken, rib, moveFace, deleteFace, helix, plane, linear/circular/curve/sketch-point patterns, mirror, boolean, split, transform, deleteBody, sheetMetal, heal |
| `import_file` | bring in STEP, Parasolid .x_t, IGES (exact solids), STL or OBJ (meshes become solids with merged flat faces); stored in the document. DXF and SVG drawings go into a sketch (new, or `sketch_id`) |
| `printability` / `send_to_slicer` | overhang and thin-wall check; stage the parts as STL in the orcaweb slicer |
| `set_part` / `set_parts` / `add_decal` | part name, colour, material, finish, hidden (one part, or a list with one rebuild); the names show in `describe` and every result; images on faces |
| `feature_defaults` | a feature kind's params with their defaults (no kind: the list of kinds) |
| `stock_size` | the raw stock each part is machined from: smallest standard plate, flat/square bar or round bar with allowances (set_part `stock` changes how) |
| `mass_properties` | exact volume, mass, centre of gravity and inertia tensor of each part (from its material, or a material/density for all) and of all of them together |
| `ray_cast` / `classify_points` | surface crossings along rays; whether points are in, on or out of the parts |
| `list_geometry` | faces and edges with refs, normals, lengths, circle centres; filter by type, box, normal or planar |
| `render` | shaded PNG from any standard or iso view (`iso_front_left`, `iso_back_right`, …) or a `direction`; sketches used by features are hidden unless `sketches: "all"` |
| `export` | STEP, IGES, STL (binary unless `ascii: true`), OBJ |
| `define_custom_feature` | scripted feature types (Rhai): parameters + selection in, solids out; use with `add_feature` kind `custom` |
| `new_part_studio` / `switch_part_studio` | more Part Studios in one document (e.g. one per part) |
| `create_assembly` / `assembly_insert` | assemblies of parts, or of other assemblies as rigid sub-assemblies |
| `list_standard_parts` / `insert_standard_part` | ISO/DIN screws, nuts, washers, pins, shafts, bearings, NEMA steppers and T-slot extrusions, built parametrically in a "Standard parts" Part Studio and inserted into an assembly |
| `assembly_mate` / `assembly_relation` / `assembly_move` | mates (fastened, revolute, slider, cylindrical, planar, ball) picked by a point on a face or edge; gear/rack/screw relations |
| `assembly_info` / `render_assembly` / `assembly_explode` | poses, BOM, interference check; shaded and exploded renders |
| `export_web` | an interactive 3D page (.html, opens in a browser) of an assembly, its slider/cylindrical/revolute mates moving within their limits and gear/rack relations following, or of a Part Studio's parts |
| `create_drawing` / `drawing_add` / `drawing_info` / `render_drawing` | Drawing tabs: views, auto or explicit dimensions (linear, radius, diameter, angle), notes, GD&T frames and datums, BOM tables, more sheets |
| `export_drawing` | PDF (all sheets)/SVG/DXF/PNG of a Drawing tab, or a quick four-view sheet of the active Part Studio |
| `create_cam` / `edit_cam` | Manufacture tabs (setups): Part Studio, bodies, up direction (`orient`, plus a 3+2 `tilt`, or an `axis` to point at the spindle), `min_rpm` for high-speed spindles, stock margins or size, material, work zero, clearance, post, machine preset or limits, `arcs`, `fixtures` (clamp boxes the toolpaths lift over), `probe` (touch plate or tool setter after each tool change), `post_text` (your own start, tool-change and end G-code), `coolant` (auto: flood on metal), `save_machine` (keep the machine settings for new setups, which otherwise start from the document's last setup); `follows` starts a setup from the stock an earlier one leaves (the part re-clamped another side up) |
| `cam_tool_library` / `cam_tool` | search the preset tool library; add a preset or a tool by kind with overrides (including a holder preset or outline), edit or delete tools |
| `import_tool_library` | import a Fusion 360 tool library (.json or .tools) or a tools.json into My tools, optionally adding every tool to a setup |
| `cam_soft_jaws` | hold a setup's part in soft jaws shaped to it (grip depth, gap, clearance, blank size, cutter); they act as fixtures, `machine_jaws` makes the tab that cuts the pocket in the blanks, or with `printed` the jaws (or with `one_piece` one block that sits on the vise bed, split by a slit with rounded ends) go to a Part Studio (`print_jaws`) and `export_step` writes them for the slicer; `mount` adds screw holes and the vise around them (`gen3_insert_bar`, `top_jaw`, or your own hole pattern) |
| `export_setup_sheet` | save an HTML setup sheet for the operator: stock, work zero, fixtures, setup pictures (iso with the vise, top and front with stock sizes), tool drawings, a toolpath picture per operation and the simulation result |
| `cam_template` | list, save, apply or delete operation templates (operations and their tools, without part-specific heights and picks) |
| `cam_op` / `cam_info` | add, edit, reorder or delete operations (2.5D, plus 3D adaptive, parallel, waterline, scallop, pencil and deburr, and trace / vcarve on the Part Studio's `sketches`; `rest` to cut only what is left); toolpath stats, cycle time and warnings |
| `cam_machine` | the mill's own geometry: `import` a STEP assembly of the machine (parts named "X axis", "Base", … are put on those axes; the spindle, table and travel are guessed), fix what carries each part (`carriers`), `rides`, `travel`, `spindle`, `table`, `holders`; `cam` makes it that setup's machine with `on_table` placing the work on the table |
| `simulate_cam` / `render_cam` | material removal simulation with crash detection (rapids into stock, shank/holder hits, gouges; with a machine model also the head, column and table against each other, the stock, the part and the fixtures, the tool against the table, and moves beyond the travel), optional PNG; toolpath render |
| `export_gcode` | G-code for GRBL, LinuxCNC, Fanuc or Mach3 |
| `create_simulation` / `edit_simulation` | Simulation tabs: linear elastic static, modal or transient analysis with a material (or each part's own, bonded where parts touch), `loss_factors`, element size, Fixed/Roller supports and Force/Pressure/Gravity loads on face refs (`at` and `radius` for a small impact patch); with no supports, modal and transient are free-free |
| `solve_simulation` / `render_simulation` | mesh and solve: max stress, deflection, factor of safety, reactions; natural frequencies, each mode's loss factor and 60 dB decay time; peak response over time; colour-map PNG on the deformed shape. Progress goes to stderr. Support and load faces are checked when the simulation is created or edited |
| `create_pcb` / `pcb_edit` | PCB tabs: a schematic and a two-layer board (size or outline, thickness, mask colour, design rules); `pcb_edit` also places parts (`at`, `rot`, `side`), adds schematic wires and labels, mounting holes and copper pours, clears routing and lays parts out (`auto_place`) |
| `pcb_add_parts` / `pcb_connect` | parts from the symbol library with their footprints; nets by pin (`{"GND": ["U1.1", "C1.2"]}`, pins by number or by name), drawn as wire stubs with labels and power/ground ports |
| `pcb_route` / `check_pcb` | the autorouter (two layers, vias) or tracks and vias by hand; the design rule check |
| `pcb_traces` | the trace solver: per net, track resistance and voltage drop, IPC-2221 heating and the width its current needs, impedance and the width its target needs; or a calculator for one track (current capacity, rise, resistance, microstrip/stripline Z0 and differential impedance, widths for a target). `pcb_edit` `copper_oz`, `er` and `rules.temp_rise` set the stackup and heating rule |
| `pcb_info` / `render_pcb` | parts with their pads, positions and nets, the netlist and airwires; PNG of the board or the schematic |
| `digikey_search` / `pcb_digikey` | Digi-Key parts: search by part number or keywords (the user's Digi-Key API client ID and secret, set in the PCB tab), or a CSV downloaded from digikey.com (`csv`); fill a part (or add a new one) with its value, footprint, MPN, manufacturer, description, datasheet, Digi-Key number and BOM fields. `pcb_edit` parts also take `manufacturer`, `description`, `datasheet`, `digikey`, `fields`, `sim` (model parameter overrides, listed per part by `pcb_info`) and `no_sim` |
| `export_pcb` / `pcb_3d` | Gerber and drill ZIP with BOM and pick-and-place CSVs; a Part Studio of the board and part bodies for assemblies and STEP |
| `pcb_sim_setup` / `simulate_pcb` | circuit simulation of the schematic: sources (default a DC supply per power port, by name), ground, pressed switches, probes; DC operating point (net voltages, part currents and power), transient (min/max/average/RMS, period, frequency, duty per probe) or AC sweep (gain, −3 dB points), with a plot PNG and CSV. Part models come from their values (see `pcb_sim_setup`). A Raspberry Pi Pico (symbol `PICO`, pins by name like `U1.GP15`) runs the firmware given in `pcb_sim_setup` `mcus` during transients; `simulate_pcb` returns its serial output with times, notes and pin states, and `V(U1.GP0)` … `V(U1.GP29)` probe its GPIOs. `pcb_sim_setup` `links` ties a DC motor (`M`), servo (`SERVO`), potentiometer (`RV`), limit switch (`SW`) or encoder (`ENC`) to an assembly mate: transients then turn the mate (probe `POS(M1)`, `RPM(M1)`), report `mechanism` and `mates_moved`, and `pose_assembly_at` leaves the assembly posed at a time for `render_assembly` |

The tool descriptions returned by `tools/list` give the full argument formats,
with a description, type and (where there is a fixed set) the allowed values of
every argument.

## Assemblies and drawings

`save_document` writes every tab, so an agent can build Part Studios, an
assembly and a drawing and hand the file to the GUI. Mate sides are given as
`{"instance": id, "face": [x, y, z]}` (or `"edge"`) with a point on the part in
its own coordinates, the same numbers `list_geometry` reports. A cylindrical
face or circular edge mates on its axis. `drawing_info` lists each view's
corner points, straight lines and circles in view coordinates, ready to pass to
`dimension` items. Dimensions and mate connectors remember the model edge or
face they were placed on and follow it when the Part Studio is rebuilt; a
dimension whose edge is gone is shown as dangling, a mate as failed.
`examples/assembly-drawing.json` walks through all of it, and
`examples/cam-plate.json` machines a plate (face, pocket, drill, thread mill,
chamfer, contour with tabs), simulates it and writes G-code.
`examples/cam-3d.json` does the same for a filleted mould block with 3D ops
(adaptive roughing, parallel and waterline finishing, deburr).
`examples/cam-setups.json` machines a part from both sides: rest roughing with a
smaller tool, pencil clean-up of the pocket's fillets, then a second setup that
flips the part and starts from the stock the first one left.
`examples/cam-engrave.json` V-carves text from a sketch into a plate and traces
a border and a line drawing from another sketch.
`examples/pico-dimmer.json` runs firmware on a Raspberry Pi Pico in a circuit: it
reads a divider on the ADC, dims an LED with PWM and prints the reading, and the
transient shows the LED current and the filtered PWM.
`examples/pico-arm.json` links a motor, limit switch, potentiometer and servo to an
assembly's mates: the Pico runs the arm to its limit switch, then sweeps the servo, and
the assembly is rendered where the arm was a quarter of a second in.

## Example

`examples/bracket.json` builds a plate whose width is the variable `w`, with a
hole and a fillet, renders it, and exports STEP, a PDF drawing and the document into the
working directory:

    rcad run examples/bracket.json

```json
[
  {"tool": "new_document", "args": {"units": "mm"}},
  {"tool": "set_variable", "args": {"name": "w", "expr": "60"}},
  {"tool": "create_sketch", "args": {"plane": "top"}},
  {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [
    {"type": "rect", "corner1": [0, 0], "corner2": [60, 40]},
    {"type": "circle", "center": [30, 20], "radius": 6}
  ]}},
  {"tool": "sketch_constrain", "args": {"sketch_id": 1, "constraints": [
    {"type": "length", "l": 5, "value": "w"},
    {"type": "diameter", "c": 10, "value": 12}
  ]}},
  {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 8}}},
  {"tool": "list_geometry", "args": {}},
  {"tool": "add_feature", "args": {"kind": "fillet", "params": {"edges": [{"kind": "edge", "point": [60, 20, 8]}], "radius": 3}}},
  {"tool": "render", "args": {"view": "iso", "path": "bracket.png"}},
  {"tool": "export", "args": {"format": "step", "path": "bracket.step"}},
  {"tool": "export_drawing", "args": {"path": "bracket.pdf", "title": "Bracket"}},
  {"tool": "save_document", "args": {"path": "bracket-part.json"}}
]
```

`{"sketch": 1}` as a profile means every region of sketch 1 except holes, so the
circle becomes a hole. The rectangle's lines are ids 5 to 8 and the circle is 10
(the `created` ids of `sketch_add`), which the constraints use.
