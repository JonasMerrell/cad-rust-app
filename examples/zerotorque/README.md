# ZeroTorque stand-in parts

ZeroTorque generates parametric putter heads. These examples run its workflow on stand-in parts,
because its own generator and STEP files are not in this repository. Run them from the repository
root.

| file | what it does |
|---|---|
| `parts.rhai` | `cad-rust-app script examples/zerotorque/parts.rhai` builds the head (lofted body with a draft, front edges filleted in one operation, cavity, plug pocket, holes), the hosel plug at lie 70° and lean 2°, and its foam core. It prints their masses, CGs and inertias, writes `Head.step`, `Plug_70_2.step`, `PlugCore_70_2.step` and `HeadAssembly.step`, and renders `head.png`. |
| `regions.rhai` | the 2D region-helper checks: disjoint unions, offsetting out and back, bridging discs, and the convex hull of discs |
| `head-cam.json` | `cad-rust-app run examples/zerotorque/head-cam.json` imports `Head.step` and programs OP1 (top: face, adaptive, parallel and waterline finishing, sight lines traced 1 mm deep with a V-bit, deburr, drill) and OP2 (sole, flipped from OP1's leftover stock: face, bore the plug pocket, drill and tap M5, deburr). It simulates both setups and posts Mach3 G-code. |
| `plug-cam.json` | the plug from the top and the underside, then a 3+2 setup that tilts the plug so that its shaft bore points at the spindle |
| `plug-modal.json` | free-free modal of the plug hollow and with its bonded foam core, and free-free taps with an impact pulse on a 3 mm patch |
| `head-modal.json` | free-free modal of the assembled head (aluminium head, 7075 plug, foam core) |

The CAM examples are set up for a 24,000 rpm, 1.5 kW high-speed spindle (`max_rpm` 24000, and
`min_rpm` 12000 because it loses torque below that) with flood coolant.
