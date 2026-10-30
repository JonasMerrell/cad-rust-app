# cad-rust-app

A desktop parametric CAD application in Rust with the same UI as `cad-app.html`
(toolbar, feature tree, variables, viewport, status bar), built on egui/eframe
with a wgpu viewport and the OpenCascade (OCCT 7.8) kernel.

## What it does

- **Documents with tabs** like Onshape: Part Studios, Assemblies and Drawings, plus named versions you can restore.
- **Part Studio:** sketches (line, rectangles, circle, arcs, tangent arc, slot, polygon, ellipse, spline, text, point; trim, extend, sketch fillet, offset, mirror, project, sketch patterns, construction, DXF and SVG import) with a constraint solver (coincident, horizontal, vertical, parallel, perpendicular, tangent, equal, midpoint, concentric, fix, symmetric) and driving dimensions that accept expressions and variables. Features: extrude (blind, symmetric, through all, up to face, second direction, add/remove/intersect), revolve, sweep (including along a helix), loft, fillet (constant/variable), chamfer, shell, hole (simple/counterbore/countersink, ISO sizes), draft, thicken, rib, move face, delete face, helix, plane (offset, angle, 3 points, normal to curve, mid plane), linear/circular pattern and mirror of parts or features, boolean, split, transform/copy, delete body, sheet metal (bent profile or plate, K-factor flat pattern DXF), STEP/IGES import. Variables and configurations, rollback bar, suppress/hide, part names, colours, materials and mass properties, measure, section view, X-ray.
- **Assemblies:** insert parts, fastened/revolute/slider/cylindrical/planar/ball mates with a mate solver, drag to move, fix/hide instances, exploded view, section view, bill of materials (CSV).
- **Parts library:** parametric standard parts (ISO/DIN socket, button, countersunk, hex and set screws; hex, thin and nyloc nuts; washers; dowel pins and shafts; ball, thrust and linear bearings; NEMA 8–23 steppers; 2020–4080 T-slot extrusions) inserted from the assembly Library, and models downloaded from GrabCAD: search opens in the browser, then insert the STEP or ZIP from the recent downloads list or by dropping it on the window.
- **Drawings:** hidden-line projected views (standard layout), section views with hatching, detail views, title block, dimensions, notes, BOM table, PDF/DXF/SVG export.
- **Manufacture (CAM):** 2.5D milling setups (orientation, 3+2 tilts for angle blocks and compound-angle features, stock, work zero, material, the spindle's speed range), a tool library with ~80 presets (flat, ball, bull nose, tapered ball, chamfer, slot, V-bits, face mills, drills, spot and centre drills, reamers, thread mills, taps) and your own tools (import Fusion 360 tool libraries, .json or .tools), operations (face, clearing, pocket, contour with tabs, chamfer, drill/peck, bore, thread mill, tap, also in a self-reversing tapping head) with suggested feeds and speeds capped to the spindle's range, 3D operations (adaptive clearing with cycloidal loops, parallel, waterline, scallop, pencil clean-up of corners a bigger tool left, and deburr/3D chamfer of picked or all sharp edges), trace and V-carve operations on sketch curves (engraved lines, text and logos carved with a V-bit), rest machining that cuts only what earlier operations left, multiple setups (flip the part and carry on from the leftover stock), tool holder outlines (ER collets, shrink-fit, side-lock), machine presets with travel, feed and tool-changer limits (and your own machine saved for new setups), setup coolant that tools inherit, fixtures (clamps, vise jaws, screws) that toolpaths lift over, automatic soft jaws (a pocket shaped to the part with clearance and corner reliefs, a tab that machines them or a Part Studio of 3D printable jaws or a one-piece printed block to export as STEP, screw holes for bolting them in the vise with presets for Gen3 insert bars and top jaws, the vise's lips and jaws as fixtures, and a check that the flipped part fits), a toolpath simulator with crash detection (rapids into stock, shank/holder collisions, fixture hits, gouges) that colours each operation's cuts, and G-code posts for GRBL, LinuxCNC, Fanuc, Mach3 and rcnc (with a **Send to rcnc** button that loads the program and its tools straight into the controller) that write arcs (G2/G3, including helices) where the toolpath runs along circles, can probe each tool on a touch plate or tool setter after a change and take your own start, tool-change and end G-code, operation templates you save once and add to any setup, and printable setup sheets for the operator (stock, work zero, fixtures, tools, operations); contours can lead on and off the wall with arcs.
- **Simulation (FEA):** linear elastic analysis of a Part Studio's parts in a Simulation tab: automatic meshing with 10-node tetrahedra, Fixed and Roller supports and Force, Pressure and Gravity loads picked on faces, static (von Mises stress, deflection, factor of safety, reaction forces), modal (natural frequencies, mode shapes, effective mass, free-free when nothing holds the part) and transient dynamic analysis (step, ramp, impact pulse or shaking loads, with damping; an unsupported part is tapped free-free), parts of different materials bonded where they touch (an aluminium shell with a foam core), material loss factors giving each mode's damping and ring-down time, shown as colour maps on the exaggerated deformed shape with a legend, probe values under the cursor and animation.
- **PCB design:** a PCB tab holds a schematic and a two-layer board that share the same parts. Place symbols from the library (resistors, capacitors, inductors, diodes, LEDs, transistors and MOSFETs, ICs, connectors, buttons, crystals), wire their pins, and name nets with labels and power/ground ports; the netlist follows the wires. On the board, footprints (0402–1206 chips, SOD-123, SOT-23/223, SOIC, TSSOP, DIP, TO-92/220, pin headers, terminal blocks and more) are placed by hand or automatically, routed by hand with 45° tracks and vias or by the autorouter, with copper pours, mounting holes, a ratsnest of what is left to connect, and a design rule check (clearances, shorts, widths, drills, board edge, overlapping parts, unrouted nets). Output: a Gerber and Excellon ZIP for board makers such as JLCPCB, BOM and pick-and-place CSVs, and **3D board**, which makes a Part Studio with the board and a body for every part to use in assemblies or export as STEP.
- **Circuit simulation:** the PCB tab's **Simulate** view runs the schematic as a SPICE-style circuit. It offers a DC operating point (every net's voltage, every part's current and power), a transient from power-on (or from the operating point), and an AC sweep (gain and phase). Parts get their models from their values: resistors, capacitors and inductors; 1N4148, 1N4007, Schottky and zener diodes; LEDs by colour; BC847/2N3904/2N2222 and BC857/2N3906 transistors; 2N7002/BSS138/AO3400/AO3401 MOSFETs; NE555 and TLC555 timers; LM358/LM324/TL072/NE5532/MCP6002 op amps; and 78xx, LM317, AMS1117 and AP2112 regulators. Power ports become DC supplies by name (VCC 5 V, +3V3 3.3 V), or you can add DC, pulse, sine and piecewise-linear sources. Click nets and parts to probe them, read their values on the schematic, and zoom the waveform plot; results export as CSV.
- **Microcontroller simulation:** a Raspberry Pi Pico in the schematic runs its firmware (a UF2, ELF, HEX or BIN built with the Pico SDK or Arduino-Pico) on an emulated RP2040 during a transient: both Cortex-M0+ cores, GPIO with pulls and interrupts, the timer, PWM, the ADC, the UARTs, the clocks and the watchdog, with the boot ROM's maths and memory functions re-implemented. The chip reads the circuit's voltages on its pins (logic levels and the ADC) and drives them back with its real drive strength, so firmware and circuit are tested together. The board brings its USB power path, 3.3 V regulator, LED on GP25 and sense dividers. The Simulate view shows the serial output with timestamps and each pin's final state. PIO, DMA, USB, SPI and I2C devices are not simulated yet. `examples/pico-dimmer.json` reads a voltage divider and dims an LED with PWM; `examples/firmware/` has the test programs and their builds.
- **Circuits that move the design:** link a DC motor, a stepper motor, a hobby servo, a potentiometer, a limit switch or an encoder in the schematic to a revolute, slider or cylindrical mate of an assembly (Simulate view, Mechanism). During a transient a motor's current turns its mate through its gearbox against the load and its speed feeds back EMF; a stepper follows the STEP pulses its driver module (A4988, DRV8825 or TMC2209 StepStick) counts at the microstep size its MS pins set, until the load or its speed asks more torque than its current gives, when it stalls and loses steps; a servo follows the 1–2 ms pulses on its signal pin; the sensors read the mate back into the circuit, so firmware can run a mechanism in closed loop. Gear, rack and screw relations carry the motion on and mate limits stop it. **Play** in the assembly's Motion section (or Play in the Simulate view) moves the parts as they moved in the run. `examples/pico-arm.json` has a Pico drive an arm through a MOSFET until its limit switch trips, reading its angle from a potentiometer, then sweep a servo.
- **Design with Claude:** the chat button in the header (or Ctrl+L) opens a panel where you describe a part or a change in plain language and Claude builds it in the open document, using the same tools as the MCP server and checking its work with renders. Connect through the Claude Code CLI on your computer (it runs `claude -p` with its own login, so requests use your Claude plan, and reaches the open document through `cad-app mcp --bridge`), an Anthropic API key, or a Claude Console account signed in with the `ant` CLI; a key is kept in `~/.config/cad-rust-app/claude.json`, readable only by you. Each request can be undone from the panel.
- **Command search** like Fusion 360: press S (/ or Ctrl+K while sketching or in a drawing, where S picks a tool), or the search button, to find and run a command by name: features, sketch tools and constraints, CAM operations, views, files and tabs.
- **Python:** `python/` builds `cadrust`, Python bindings to the same kernel with a FreeCAD-style API, offscreen renders, STEP assemblies with names and colours, interactive web pages, and finite elements with a mesh size per part (see [python/README.md](python/README.md)).
- **Scripting:** `cad-rust-app script file.rhai` runs a Rhai script with no window on the same kernel: shapes, 2D regions and offsets, many-tool and fuzzy booleans, fillets on chosen edges, mass properties, point classification, ray casting, numeric helpers and renders. See [SCRIPTING.md](SCRIPTING.md); `examples/zerotorque/` builds a putter head and machines and analyses it.
- **Export:** STEP, IGES, STL, OBJ, PNG, G-code, and an interactive web page (one .html file) of an assembly whose slider and revolute mates move within their limits.

## Build and run

    cargo build --release        # first build compiles OpenCascade (~20 min)
    ./target/release/cad-rust-app [part.json]

Built from source the program is `cad-rust-app`; the release archives ship it as
`cad-app` (with `AGENTS.md`, `SCRIPTING.md`, `.mcp.json` and `examples/`), and
the agent and scripting docs use that name. `--help` lists the command line
modes and `--version` prints the version.

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
| `src/stdparts.rs`, `src/stdparts_ui.rs` | standard parts library (dimension tables as custom feature scripts) and its window |
| `src/grabcad.rs` | GrabCAD search link, ZIP reading, recent downloads |
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
| `src/pcb.rs` | PCB model: parts, schematic wires and labels, the netlist, board copper, ratsnest, copper pours and the design rule check |
| `src/pcb_lib.rs` | schematic symbols, footprints and the silkscreen stroke font |
| `src/pcb_route.rs` | the grid autorouter and automatic placement |
| `src/pcb_export.rs` | drawing lists for both views, Gerber/Excellon/BOM/CPL output and the 3D board Part Studio |
| `src/pcb_ui.rs` | PCB tab |
| `src/circuit.rs` | circuit simulation setup, part models by value, the circuit built from the schematic, results and measurements |
| `src/circuit_solve.rs` | the circuit solver: MNA with Newton-Raphson, variable-step BDF2 transient with events and an external pin driver, AC sweep |
| `src/circuit_mcu.rs` | the Raspberry Pi Pico board in a circuit and its firmware running alongside the transient |
| `src/circuit_mech.rs` | motors, servos and sensors linked to assembly mates, and the mechanism running alongside the transient |
| `src/mcu_cm0.rs` / `src/mcu_rp2040.rs` / `src/mcu_rom.rs` | the Cortex-M0+ core, the RP2040 (memory, peripherals, firmware files) and its boot ROM re-implemented |
| `src/circuit_plot.rs` / `src/circuit_ui.rs` | waveform plots; the PCB tab's Simulate view |
| `src/agent.rs` | headless tool interface: MCP server, `run` scripts, and the tools the Claude panel uses |
| `src/agent_schema.rs` | the tools' argument schemas and the check that holds calls to them |
| `src/agent_query.rs` | mass properties, point classification and ray casting for the agent tools (run in the kernel worker) |
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

Copyright (C) 2026 Jonas Merrell.

This project is licensed under the [GNU Affero General Public License v3.0 only](LICENSE)
(`AGPL-3.0-only`). You may use, modify and redistribute it, but if you distribute it, or
let people use a modified version over a network, you must make the complete corresponding
source available under the same license.

The exceptions are the vendored
`crates/opencascade` and `crates/opencascade-sys` bindings, which keep their original
LGPL-2.1 license. The app also links [Open CASCADE Technology](https://dev.opencascade.org/),
which is LGPL-2.1 with an additional exception; anyone distributing binaries of the app
must meet the LGPL terms for those parts (for example by pointing to their source and
allowing them to be replaced).
