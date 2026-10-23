# Using cad-app from an AI agent

cad-app has a headless interface with the same modelling kernel as the editor.
Agents can drive it in two ways:

- **MCP server** (`cad-app mcp`): a Model Context Protocol server on stdio.
- **Script runner** (`cad-app run script.json`): runs a JSON list of tool calls
  and prints one JSON result per line. This suits agents without MCP and CI.

Files saved with `save_document` open in the GUI (`cad-app part.json`).

## Setup

Claude Code (user-wide):

    claude mcp add --scope user cad -- cad-app mcp

Or use the `.mcp.json` in this repository, which Claude Code picks up when it
runs in this folder. Other MCP clients start the same command
(`cad-app mcp`, transport stdio).

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
| `import_file` | bring in STEP, IGES (exact solids), STL or OBJ (meshes become solids with merged flat faces); stored in the document. DXF and SVG drawings go into a sketch (new, or `sketch_id`) |
| `printability` / `send_to_slicer` | overhang and thin-wall check; stage the parts as STL in the orcaweb slicer |
| `set_part` / `add_decal` | part name, colour, material, finish, hidden; images on faces |
| `list_geometry` | faces and edges with refs, normals, lengths, circle centres; filter by type, box, normal or planar |
| `render` | shaded PNG from any standard or iso view (`iso_front_left`, `iso_back_right`, …) or a `direction`; sketches used by features are hidden unless `sketches: "all"` |
| `export` | STEP, IGES, STL (binary unless `ascii: true`), OBJ |
| `define_custom_feature` | scripted feature types (Rhai): parameters + selection in, solids out; use with `add_feature` kind `custom` |
| `new_part_studio` / `switch_part_studio` | more Part Studios in one document (e.g. one per part) |
| `create_assembly` / `assembly_insert` | assemblies of parts, or of other assemblies as rigid sub-assemblies |
| `assembly_mate` / `assembly_relation` / `assembly_move` | mates (fastened, revolute, slider, cylindrical, planar, ball) picked by a point on a face or edge; gear/rack/screw relations |
| `assembly_info` / `render_assembly` / `assembly_explode` | poses, BOM, interference check; shaded and exploded renders |
| `create_drawing` / `drawing_add` / `drawing_info` / `render_drawing` | Drawing tabs: views, auto or explicit dimensions (linear, radius, diameter, angle), notes, GD&T frames and datums, BOM tables, more sheets |
| `export_drawing` | PDF (all sheets)/SVG/DXF/PNG of a Drawing tab, or a quick four-view sheet of the active Part Studio |
| `create_cam` / `edit_cam` | Manufacture tabs (setups): Part Studio, bodies, up direction, stock margins or size, material, work zero, clearance, post, machine preset or limits; `follows` starts a setup from the stock an earlier one leaves (flip the part) |
| `cam_tool_library` / `cam_tool` | search the preset tool library; add a preset or a tool by kind with overrides (including a holder preset or outline), edit or delete tools |
| `cam_op` / `cam_info` | add, edit, reorder or delete operations (2.5D, plus 3D adaptive, parallel, waterline, scallop, pencil and deburr, and trace / vcarve on the Part Studio's `sketches`; `rest` to cut only what is left); toolpath stats, cycle time and warnings |
| `simulate_cam` / `render_cam` | material removal simulation with crash detection (rapids into stock, shank/holder hits, gouges), optional PNG; toolpath render |
| `export_gcode` | G-code for GRBL, LinuxCNC, Fanuc or Mach3 |

The tool descriptions returned by `tools/list` give the full argument formats.

## Assemblies and drawings

`save_document` writes every tab, so an agent can build Part Studios, an
assembly and a drawing and hand the file to the GUI. Mate sides are given as
`{"instance": id, "face": [x, y, z]}` (or `"edge"`) with a point on the part in
its own coordinates, the same numbers `list_geometry` reports. A cylindrical
face or circular edge mates on its axis. `drawing_info` lists each view's
corner points, straight lines and circles in view coordinates, ready to pass to
`dimension` items. `examples/assembly-drawing.json` walks through all of it, and
`examples/cam-plate.json` machines a plate (face, pocket, drill, thread mill,
chamfer, contour with tabs), simulates it and writes G-code.
`examples/cam-3d.json` does the same for a filleted mould block with 3D ops
(adaptive roughing, parallel and waterline finishing, deburr).
`examples/cam-setups.json` machines a part from both sides: rest roughing with a
smaller tool, pencil clean-up of the pocket's fillets, then a second setup that
flips the part and starts from the stock the first one left.
`examples/cam-engrave.json` V-carves text from a sketch into a plate and traces
a border and a line drawing from another sketch.

## Example

`examples/bracket.json` builds a plate with a hole and a fillet, renders it,
and exports STEP, a PDF drawing and the document:

    cad-app run examples/bracket.json

```json
[
  {"tool": "new_document", "args": {"units": "mm"}},
  {"tool": "create_sketch", "args": {"plane": "top"}},
  {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [
    {"type": "rect", "corner1": [0, 0], "corner2": [60, 40]},
    {"type": "circle", "center": [30, 20], "radius": 6}]}},
  {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 8}}},
  {"tool": "add_feature", "args": {"kind": "fillet", "params": {"edges": [{"kind": "edge", "point": [60, 20, 8]}], "radius": 3}}},
  {"tool": "render", "args": {"view": "iso", "path": "bracket.png"}},
  {"tool": "export", "args": {"format": "step", "path": "bracket.step"}}
]
```

`{"sketch": 1}` as a profile means every region of sketch 1 except holes, so the
circle becomes a hole.
