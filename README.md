# cad-rust-app

A desktop parametric CAD application in Rust with the same UI as `cad-app.html`
(toolbar, feature tree, variables, viewport, status bar), built on egui/eframe
with a wgpu viewport and the OpenCascade (OCCT 7.8) kernel.

## What it does

- **Documents with tabs** like Onshape: Part Studios, Assemblies and Drawings, plus named versions you can restore.
- **Part Studio:** sketches (line, rectangles, circle, arcs, tangent arc, slot, polygon, ellipse, spline, text, point; trim, extend, sketch fillet, offset, mirror, project, sketch patterns, construction, DXF and SVG import) with a constraint solver (coincident, horizontal, vertical, parallel, perpendicular, tangent, equal, midpoint, concentric, fix, symmetric) and driving dimensions that accept expressions and variables. Features: extrude (blind, symmetric, through all, up to face, second direction, add/remove/intersect), revolve, sweep (including along a helix), loft, fillet (constant/variable), chamfer, shell, hole (simple/counterbore/countersink, ISO sizes), draft, thicken, rib, move face, delete face, helix, plane (offset, angle, 3 points, normal to curve, mid plane), linear/circular pattern and mirror of parts or features, boolean, split, transform/copy, delete body, sheet metal (bent profile or plate, K-factor flat pattern DXF), STEP/IGES import. Variables and configurations, rollback bar, suppress/hide, part names, colours, materials and mass properties, measure, section view, X-ray.
- **Assemblies:** insert parts, fastened/revolute/slider/cylindrical/planar/ball mates with a mate solver, drag to move, fix/hide instances, exploded view, section view, bill of materials (CSV).
- **Drawings:** hidden-line projected views (standard layout), section views with hatching, detail views, title block, dimensions, notes, BOM table, PDF/DXF/SVG export.
- **Manufacture (CAM):** 2.5D milling setups (orientation, stock, work zero, material), a tool library with ~80 presets (flat, ball, bull nose, tapered ball, chamfer, slot, V-bits, face mills, drills, spot and centre drills, reamers, thread mills, taps) and your own tools, operations (face, clearing, pocket, contour with tabs, chamfer, drill/peck, bore, thread mill, tap) with suggested feeds and speeds, 3D operations (adaptive clearing with cycloidal loops, parallel, waterline, scallop, pencil clean-up of corners a bigger tool left, and deburr/3D chamfer of picked or all sharp edges), trace and V-carve operations on sketch curves (engraved lines, text and logos carved with a V-bit), rest machining that cuts only what earlier operations left, multiple setups (flip the part and carry on from the leftover stock), tool holder outlines (ER collets, shrink-fit, side-lock), machine presets with travel, feed and tool-changer limits, fixtures (clamps, vise jaws, screws) that toolpaths lift over, a toolpath simulator with crash detection (rapids into stock, shank/holder collisions, fixture hits, gouges) that colours each operation's cuts, and G-code posts for GRBL, LinuxCNC, Fanuc and Mach3 that write arcs (G2/G3, including helices) where the toolpath runs along circles; contours can lead on and off the wall with arcs.
- **Export:** STEP, IGES, STL, OBJ, PNG, G-code.

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
