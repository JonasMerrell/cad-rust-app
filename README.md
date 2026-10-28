# cad-rust-app

A desktop parametric CAD application in Rust with the same UI as `cad-app.html`
(toolbar, feature tree, variables, viewport, status bar), built on egui/eframe
with a wgpu viewport and the OpenCascade (OCCT 7.8) kernel.

## What it does

- **Documents with tabs** like Onshape: Part Studios, Assemblies and Drawings, plus named versions you can restore.
- **Part Studio:** sketches (line, rectangles, circle, arcs, tangent arc, slot, polygon, ellipse, spline, text, point; trim, extend, sketch fillet, offset, mirror, project, sketch patterns, construction, DXF and SVG import) with a constraint solver (coincident, horizontal, vertical, parallel, perpendicular, tangent, equal, midpoint, concentric, fix, symmetric) and driving dimensions that accept expressions and variables. Features: extrude (blind, symmetric, through all, up to face, second direction, add/remove/intersect), revolve, sweep (including along a helix), loft, fillet (constant/variable), chamfer, shell, hole (simple/counterbore/countersink, ISO sizes), draft, thicken, rib, move face, delete face, helix, plane (offset, angle, 3 points, normal to curve, mid plane), linear/circular pattern and mirror of parts or features, boolean, split, transform/copy, delete body, sheet metal (bent profile or plate, K-factor flat pattern DXF), STEP/IGES import. Variables and configurations, rollback bar, suppress/hide, part names, colours, materials and mass properties, measure, section view, X-ray.
- **Assemblies:** insert parts, fastened/revolute/slider/cylindrical/planar/ball mates with a mate solver, drag to move, fix/hide instances, exploded view, section view, bill of materials (CSV).
- **Drawings:** hidden-line projected views (standard layout), section views with hatching, detail views, title block, dimensions, notes, BOM table, PDF/DXF/SVG export.
- **Manufacture (CAM):** 2.5D milling setups (orientation, 3+2 tilts for angle blocks and compound-angle features, stock, work zero, material, the spindle's speed range), a tool library with ~80 presets (flat, ball, bull nose, tapered ball, chamfer, slot, V-bits, face mills, drills, spot and centre drills, reamers, thread mills, taps) and your own tools (import Fusion 360 tool libraries, .json or .tools), operations (face, clearing, pocket, contour with tabs, chamfer, drill/peck, bore, thread mill, tap) with suggested feeds and speeds, 3D operations (adaptive clearing with cycloidal loops, parallel, waterline, scallop, pencil clean-up of corners a bigger tool left, and deburr/3D chamfer of picked or all sharp edges), trace and V-carve operations on sketch curves (engraved lines, text and logos carved with a V-bit), rest machining that cuts only what earlier operations left, multiple setups (flip the part and carry on from the leftover stock), tool holder outlines (ER collets, shrink-fit, side-lock), machine presets with travel, feed and tool-changer limits, fixtures (clamps, vise jaws, screws) that toolpaths lift over, automatic soft jaws (a pocket shaped to the part with clearance and corner reliefs, a tab that machines them or a Part Studio of 3D printable jaws to export as STEP, and a check that the flipped part fits), a toolpath simulator with crash detection (rapids into stock, shank/holder collisions, fixture hits, gouges) that colours each operation's cuts, and G-code posts for GRBL, LinuxCNC, Fanuc and Mach3 that write arcs (G2/G3, including helices) where the toolpath runs along circles, can probe each tool on a touch plate or tool setter after a change and take your own start, tool-change and end G-code, operation templates you save once and add to any setup, and printable setup sheets for the operator (stock, work zero, fixtures, tools, operations); contours can lead on and off the wall with arcs.
- **Simulation (FEA):** linear elastic analysis of a Part Studio's parts in a Simulation tab: automatic meshing with 10-node tetrahedra, Fixed and Roller supports and Force, Pressure and Gravity loads picked on faces, static (von Mises stress, deflection, factor of safety, reaction forces), modal (natural frequencies, mode shapes, effective mass, free-free when nothing holds the part) and transient dynamic analysis (step, ramp, impact pulse or shaking loads, with damping; an unsupported part is tapped free-free), parts of different materials bonded where they touch (an aluminium shell with a foam core), material loss factors giving each mode's damping and ring-down time, shown as colour maps on the exaggerated deformed shape with a legend, probe values under the cursor and animation.
- **Design with Claude:** the chat button in the header (or Ctrl+L) opens a panel where you describe a part or a change in plain language and Claude builds it in the open document, using the same tools as the MCP server and checking its work with renders. Connect an Anthropic API key or a Claude Console account signed in with the `ant` CLI; the key is kept in `~/.config/cad-rust-app/claude.json`, readable only by you. Each request can be undone from the panel. A Claude Pro or Max plan can't be signed in to from other apps, so to use one, add `cad-app mcp` to Claude Desktop or Claude Code (see [AGENTS.md](AGENTS.md)).
- **Command search** like Fusion 360: press S (/ or Ctrl+K while sketching or in a drawing, where S picks a tool), or the search button, to find and run a command by name: features, sketch tools and constraints, CAM operations, views, files and tabs.
- **Python:** `python/` builds `cadrust`, Python bindings to the same kernel with a FreeCAD-style API, offscreen renders, STEP assemblies with names and colours, interactive web pages, and finite elements with a mesh size per part (see [python/README.md](python/README.md)).
- **Scripting:** `cad-rust-app script file.rhai` runs a Rhai script with no window on the same kernel: shapes, 2D regions and offsets, many-tool and fuzzy booleans, fillets on chosen edges, mass properties, point classification, ray casting, numeric helpers and renders. See [SCRIPTING.md](SCRIPTING.md); `examples/zerotorque/` builds a putter head and machines and analyses it.
- **Export:** STEP, IGES, STL, OBJ, PNG, G-code, and an interactive web page (one .html file) of an assembly whose slider and revolute mates move within their limits.

## Build and run

    cargo build --release        # first build compiles OpenCascade (~20 min)
    ./target/release/cad-rust-app [part.json]

The modelling kernel runs in a child process (`--kernel-worker`) so a failing
OpenCascade operation can't crash the editor; the feature that crashed is
marked with an error and the rest of the part still rebuilds.

## Layout

| file | what |
|---|---|
| `src/app.rs` | app state, toolbar, feature tree, variables, status bar, undo/redo, files |
| `src/viewport.rs` | navigation, scene assembly, picking, measure, overlays |
| `src/render.rs` | wgpu pipelines (shaded meshes, thick lines, points, section clipping) |
| `src/sketcher.rs` | sketch mode tools, constraints, dimensions |
| `src/sketch_import.rs` | DXF and SVG drawings into sketches |
| `src/sketch.rs` | sketch data model and constraint solver |
| `src/regions.rs` | closed-region detection (planar arrangement) |
| `src/dialog.rs` | feature dialog |
| `src/doc.rs` | document model (features, references, variables), JSON format |
| `src/kernel.rs` | feature evaluation on OpenCascade, meshing |
| `src/worker.rs` | kernel worker process protocol |
| `src/expr.rs` | expressions and units |
| `src/cam.rs` | CAM model: tools, presets, operations, setups |
| `src/cam_paths.rs` | 2.5D toolpath generation (shadow slicing, offsets, holes) |
| `src/cam_3d.rs` | 3D toolpaths: drop cutter, adaptive, parallel, waterline, scallop, pencil, deburr |
| `src/cam_engrave.rs` | trace and V-carve along sketch curves |
| `src/cam_sim.rs` | heightmap simulator and collision checks |
| `src/cam_stock.rs` | leftover stock between operations and setups (rest machining, flipping) |
| `src/cam_post.rs` | G-code post processors |
| `src/cam_ui.rs` | Manufacture tab |
| `src/fea.rs` | Simulation model (supports, loads, materials) and running a study |
| `src/fea_mesh.rs` | automatic tetrahedral meshing (isosurface stuffing, 10-node elements) |
| `src/fea_solve.rs` | linear elastic solver: static, modal (Lanczos) and transient (Newmark) |
| `src/fea_ui.rs` | Simulation tab |
| `src/agent.rs` | headless tool interface: MCP server, `run` scripts, and the tools the Claude panel uses |
| `src/claude.rs` | Claude Messages API client and the tool loop that runs on a background thread |
| `src/claude_ui.rs` | Claude chat panel |
| `src/script_api.rs` | headless scripting API (shapes, topology, queries, files) |
| `src/script_num.rs` | scripting numerics, ray casting, accessibility and renders |
| `crates/` | vendored `opencascade`/`opencascade-sys` bindings (internals made public) |
| `tools/gui/` | headless Xvfb rig for screenshots |
| `ref/` | material extracted from the original HTML app |

## Controls

Right-drag orbits, middle-drag (or Shift+right-drag) pans, the wheel zooms at the
cursor. Keys: Shift+S/E/R/F/H start Sketch/Extrude/Revolve/Fillet/Hole, 0–3 and F
for views, M measure, Ctrl+Z / Ctrl+Shift+Z undo/redo, Ctrl+S save. In a sketch:
L line, R rectangle, C circle, A arc, P point, D dimension, K trim, O offset,
U project, Q construction, G grid snap, N normal-to; with a selection,
I/H/V/T/E/M/X apply constraints; Enter finishes, Esc steps back.

## License

This project is licensed under the [MIT License](LICENSE), except for the vendored
`crates/opencascade` and `crates/opencascade-sys` bindings, which keep their original
LGPL-2.1 license. The app also links [Open CASCADE Technology](https://dev.opencascade.org/),
which is LGPL-2.1 with an additional exception; anyone distributing binaries of the app
must meet the LGPL terms for those parts (for example by pointing to their source and
allowing them to be replaced).
