# Cutting model (stock simulation load check)

What rCAD's stock simulation works out for every cutting tool position, how, with which constants, and
what it leaves out. Code: `src/cam_cutting.rs` (the physics), `src/cam_sim.rs` (`load_sample`, `OpLoad`,
`air_sample`, `unfinished`). It is a model for catching cuts a machine won't take and for comparing
toolpaths; numbers are within maybe ±30% unless calibrated, trends are more reliable than absolutes.

## Measured from the stock model

The stock is a height map (cells about 0.13 mm at the default resolution). The tool is stepped along each
feed move about a cell at a time; each step lowers the cells under the cutter's shape and records the volume
removed and the deepest cut.

- **Cross-section** A (mm² per mm of travel) = volume removed over the last ~1 mm of travel ÷ that travel
  (travel along the path, so a climb up a steep face counts along the face).
- **Axial depth** ap = the deepest cut in that window; **radial engagement** ae = A / ap (at most the
  diameter). Plunges and ramps down steeper than 1:2 are left out (they cut with the end).
- Limitation: the height map sees material from above only, so on a steep wall a column's whole height
  above the tool counts as engaged (ap can read high for side cuts high up a wall).

## Kinematics

- Feed per tooth fz = vf / (n · z) (vf the programmed feed of the move, n the setup's spindle speed for the
  tool, z flutes). Acceleration and corner slow-down are not applied yet (rcnc's planner runs G64 with
  0.01 mm junction deviation; real feed in tight 3D work is lower, so loads there are slightly pessimistic).
- Ball end mills cut with the diameter at the engaged depth: d = 2·√(ap·(D − ap)) for ap < D/2.
- Engagement angle φ = arccos(1 − 2·ae/d) (a slot: 180°).
- Chip thickness: h_max = fz·sin φ (φ < 90°) or fz; mean h_m = fz·(1 − cos φ)/φ (radial chip thinning).

## Forces, power, torque (Kienzle)

- Specific cutting force kc(h) = kc1 · h^(−mc), h in mm (floored at 0.005 mm; thinner is rubbing).
  6061-T6: kc1 = 700 N/mm², mc = 0.25 (Sandvik CMC N1.2 class, rounded). Other materials in
  `cam_cutting::material`; the stock's material picks them.
- Teeth in the cut: z·φ/2π on average.
- Mean tangential force F_t = kc(h_m) · ap · h_m · z·φ/2π. Straight-tooth peak F_peak = kc(h_max)·ap·h_max·⌈z·φ/2π⌉
  (kept as an upper bound).
- Sideways force on the tool (for deflection, stress, the machine): F = max(F_t, F_peak/4) · 1.3 · √(1 + kr²),
  the 1.3 for the helix spreading each tooth's cut, kr = 0.35 radial/tangential for aluminium.
- Cutting speed vc = π·d·n (m/min); power P = F_t·vc/60 (W); torque T = F_t·d/2.

## The spindle

VFD spindle: rated electrical input P_in at its rated speed, efficiency η (shaft power = η·P_in), constant
torque below rated speed, so the power available at n is η·P_in·min(1, n/n_rated). Machine config
(`Machine::spindle_power`, `spindle_efficiency`, `spindle_rated_rpm`); unset means 1.5 kW, 0.78, 24000 rpm,
reported as assumed. The DMC2: 1.5 kW input, about 1170 W at the shaft; 490 W at 10 000 rpm.

## The tool

- Stepped cantilever from the holder: shank (its diameter) then the fluted length (bending as about 80% of
  the diameter), loaded at the middle of the cut. Tip deflection δ = F/(3E)·[(L³ − Lf³)/I_shank + Lf³/I_flutes].
  E: carbide 600 GPa, HSS/cobalt 210 GPa. The stick-out is the tool's `stickout`: get it right on the machine.
- Bending stress at the holder (or where the flutes start, if that is weaker): σ = M·c/I; limits 800 MPa
  carbide, 900 MPa HSS (well under rupture: fatigue and edge chipping come first).
- Chip load: h_max against 2.5% of the diameter.
- The tool's material comes from the library. Jonas's tools are all carbide (the 1/4" flats DLC coated);
  his Fusion library's material column is wrong for some, so the jobs import it with `material: carbide`.

## Limits and what is reported

Each as a share of its limit (1 = at it): spindle power and torque, sideways force (machine: 700 N assumed
until measured; the DMC2 took 576 N), deflection (0.06 mm for roughing; finishing passes report it as a wall
error and don't count it as load), bending stress, chip load. The operation's load is the worst share at its
heaviest point. Above 85%: caution; above 100%: an Overload problem. Generated toolpaths should aim at about
70% of the spindle power or less.

The older geometric rule (allowed depth for the width engaged, cross-section ≤ 0.4 D² carbide / 0.24 D² HSS)
is still reported as `geometric`.

Also per operation: the share of feed time cutting nothing (air) or next to nothing (under 5% of its own
step-down × stepover), and machined part surface left more than 0.1 mm proud (roughing no finishing pass
followed).

## Calibration (DMC2, 6061-T6, 1/4" 3-flute carbide at 24 000 rpm, 24 out of the holder)

| Cut | Observed | Model |
|---|---|---|
| 0.6 wide × 12 deep, 2500 mm/min | ran comfortably | 656 W, 56% of the spindle |
| 4.65 × 3, 2400 mm/min (test part) | ran, sounded close to bogging | 995 W, 85% (caution) |
| 1 × 12.5 finish pass, 2400 (phone stand op 1) | ran, sounded close to bogging | 1068 W, 91% (caution) |
| 5 × 4 round a corner, 2400 (phone stand op 1) | audibly bogged | 1422 W, 122% (problem) |

These are the tests `cam_cutting::tests::calibrated_on_the_dmc2` and
`cam_3d::tests::load_check_flags_a_contour_into_square_corners_not_adaptive`. Add every new observation the
same way.

## Not modelled

Chatter and stability lobes (planned), chip evacuation (planned), tool wear and edge rounding, thermal
effects, the machine's own stiffness and the workholding (a part can move before the tool bends), the
spindle's speed droop under load (planned with the rcnc microphone), runout.

## Better calibration

The spindle's power is the binding limit on the DMC2, so the best measurement is spindle power or current
under load: a clamp meter on one motor phase or the VFD's own output-current display, read during a steady
cut of known width/depth/feed. Next best: spindle speed droop (a tachometer, or the planned microphone's
tooth-passing frequency). Each reading with its cut goes in as a calibration test.
