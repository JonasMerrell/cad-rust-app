# cad-rust-app

A desktop parametric CAD application in Rust with the same UI as `cad-app.html`
(toolbar, feature tree, variables, viewport, status bar), built on egui/eframe
with a wgpu viewport and the OpenCascade (OCCT 7.8) kernel.

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
| `src/sketch.rs` | sketch data model and constraint solver |
| `src/regions.rs` | closed-region detection (planar arrangement) |
| `src/dialog.rs` | feature dialog |
| `src/doc.rs` | document model (features, references, variables), JSON format |
| `src/kernel.rs` | feature evaluation on OpenCascade, meshing |
| `src/worker.rs` | kernel worker process protocol |
| `src/expr.rs` | expressions and units |
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
