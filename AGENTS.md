# Using cad-app from an AI agent

cad-app has a headless interface with the same modelling kernel as the editor.
Agents can drive it in two ways:

- **MCP server** (`cad-app mcp`): a Model Context Protocol server on stdio.
- **Script runner** (`cad-app run script.json`): runs a JSON list of tool calls
  and prints one JSON result per line. This suits agents without MCP and CI.

Files saved with `save_document` open in the GUI (`cad-app part.json`).

## Setup

Claude Code (user-wide):

    claude mcp add --scope user cad -- /home/jonas/.local/bin/cad-app mcp

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
| `add_feature` / `edit_feature` / `delete_feature` | extrude, revolve, sweep, loft, fillet, chamfer, shell, hole, draft, thicken, rib, moveFace, deleteFace, helix, plane, patterns, mirror, boolean, split, transform, deleteBody, sheetMetal |
| `list_geometry` | faces and edges with refs, normals, lengths, circle centres |
| `render` | shaded PNG from iso/front/top/right/back/bottom/left |
| `export` | STEP, IGES, STL, OBJ |
| `export_drawing` | PDF/SVG/DXF drawing with front, top, right and iso views |

The tool descriptions returned by `tools/list` give the full argument formats.

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
