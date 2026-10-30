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

## Controller firmware

`controller/arm_controller.c` runs the arm from a Raspberry Pi Pico on a ring-shaped board inside the
base: J1–J3 on A4988 StepStick drivers (STEP/DIR on GP2–GP7, shared enable on GP8, eighth steps),
home switches on GP10–GP12, servo PWM for J4, J5 and the gripper on GP13–GP15, and the 24 V motor
supply read through a 100k/10k divider on GP26. It waits for the supply, homes J1–J3 together, then
runs a pick-and-place cycle of coordinated trapezoidal moves. `arm_controller.uf2` is a build of it;
in the PCB tab's simulation the Pico runs it, the drivers count its steps into the arm's mates and
report how much of each stepper's torque the moves use. To rebuild (Pico SDK 2.x):

    PICO_SDK_PATH=/path/to/pico-sdk cmake -S examples/robot-arm/controller -B build -G Ninja -DPICO_BOARD=pico
    ninja -C build
