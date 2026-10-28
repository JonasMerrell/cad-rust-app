# 5-axis desktop robot arm

`parts.rhai` builds a desktop robot arm with the scripting API: base yaw, shoulder, elbow, wrist pitch
and wrist roll, plus a gear-coupled two-finger gripper.

    mkdir out && cad-rust-app script examples/robot-arm/parts.rhai out

It writes one STEP per part in that part's own frame (the machined parts lie flat, ready for a
Manufacture tab), `robot-arm-assembly.step`, `Shoulder-yoke-assembly.step` (yoke base and both cheeks,
for a bonded FEA), two renders and `placements.json`. `placements.json` lists every instance (part,
`rotate` axis and angle, `position`, as `assembly_insert` takes them), each part's mass, and the holding
torques J2 and J3 need with the arm stretched out and a 500 g payload.

- Machined, 6061: bottom plate, top plate, turntable, yoke base, yoke cheeks (2), upper arm, forearm.
  Every one is 2.5D work from plate stock: the plates in one setup with tabs, the others flipped
  into printed soft jaws for the second side, and the cheeks in a third setup for two tapped holes in
  their bottom edge.
- 3D printed, PETG: base shroud, wrist link, gripper palm, two fingers.
- Bought (outline stand-ins): NEMA17 steppers with Ø36 planetary gearboxes (J1–J3), MG996R servos
  (J4, J5), an SG90 (gripper), flange hubs for Ø8 shafts, a 51108 thrust bearing, a 608 bearing,
  M5 × 75 standoffs and socket head screws.
