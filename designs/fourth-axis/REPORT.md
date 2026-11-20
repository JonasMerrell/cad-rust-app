# DMC2 rotary A axis: design report

Written 2026-11-20. Everything here is modelled and calculated; nothing has been built, bought or run. Tags used throughout: **D** vendor datasheet, **C** calculated in `scripts/calc.py`, **F** rCAD finite elements, **G** guess or typical value.

## 1. Result in short

- **What it is:** a rotary A axis along X that bolts to the M8 fixture grid in place of the two-station vise. A bought size-25 strain-wave reducer (100:1) drives a spindle that runs in a bought 120 mm crossed-roller bearing; a pneumatic disc brake clamps the spindle for indexed roughing; a closed-loop NEMA 34 stepper drives it. The XinDian H54-8-55 self-centring vise sits on a removable faceplate with its clamp centre on the axis. A plain tailstock with a dead centre supports long parts.
- **Envelope:** centre height 77.2 mm above the table. The vise swings Ø92 mm. Parts up to about Ø90 mm can be machined on top; the tool reaches down to centre height on parts up to about Ø66 mm (the collet nut is the limit, section 8). Between the jaw face and the tail centre: up to about 120 mm. The vise grips 8 to 55 mm.
- **Stiffness at the mean roughing force (391 N):** 13.7 µm at the jaws (target 20). Finishing force: 2.73 µm (target 5). At 100 mm overhang without the tailstock it is 54.4 µm, so long parts need the tailstock (13.2 µm mid-length on a Ø25 x 120 aluminium bar).
- **Torsion:** with the brake on, 1.1 µm at the 46 mm radius while roughing. On the reducer alone, 24.6 µm roughing and 5.3 µm finishing at 46 mm (1.6 µm on a Ø50 part).
- **Backlash:** the reducer's rating is 20 arc-sec at most (D), with 60 arc-sec of hysteresis on a torque reversal (D). Approaching from one side, repeatability is about 11 arc-sec (2.6 µm at 46 mm). No keys, splines or gears of our own are in the torque path: every joint we make is a clamped flat face.
- **Makeable:** 10 of 10 machined parts pass an automatic check for one 1/4 in end mill from two sides (section 9). No tapped holes, no holes under 6.5 mm, no press fits. Two parts are printed without supports.
- **Not verified:** nothing is measured. The bearing's stiffness, the bolted joints, the brake's friction and the shop air pressure are calculated or assumed (section 12).

### Decisions and questions for Jonas

1. **It replaces the vise.** The two-station vise covers 318 of the table's 350 mm, so the two cannot share the table.
2. **Air:** the brake needs about 6 bar and one controller output. If that is not wanted, the same disc can be clamped by hand through the port (not designed here), or the axis runs on the reducer alone with roughing kept to smaller radii.
3. **Cost:** the reducer is 269 USD (under the 300 USD line). The separate main bearing adds about 180 USD for a P5 clone, 700 to 820 USD for a real THK. Without it (iteration 1) the axis is about ten times softer at the jaws.
4. **NEMA 34, not NEMA 23:** the size-25 reducer takes a 14 mm keyed shaft, which is the NEMA 34 shaft. It needs the CL86T drive. The mill's own axes are NEMA 23 by the CAD.
5. **Tool measuring with the axis mounted:** the headstock stands 142 mm above the table and a retracted 24 mm tool tip only 126 mm, so a loaded tool cannot cross the headstock. rcnc needs a keep-out and a route (section 11).
6. **Left tub wall:** the motor ends 68 mm past the table's left edge, 3.9 mm short of the wall in the CAD. Measure the real gap before buying.
7. **Fixture grid:** the grid's hole positions are not in the CAD. The base plates use 25 mm spacings, so they land on either grid, but the axis position is only known to +/- 12.5 mm.

## 2. What was measured from the machine model

From `~/.config/cad-rust-app/machines/DMC2.json` and the original STEP (model coordinates, mm):

| Item | Value |
|---|---|
| Table | 350 x 250, x -127.2 to 222.8, y -475.3 to -225.3, top z 11.24 |
| Travel | X -123.7 to 219.3 (343), Y -150.7 to 83.1 (234), Z -135.3 to 14.7 (150) |
| Spindle (collet nut face) | x 0, y -310.07, z 146.54 at the modelled position; 150 mm above the table at Z top |
| Tool tip above the table at Z top | 126 mm with the usual 24 mm stick-out |
| Spindle reach over the table in Y | only the back 168 mm (table y -393 to -225) |
| Two-station vise | 318 x 145 mm, jaw tops 28 mm above the table |
| Tub walls | inner faces at x -199.2 and 294.8 above the table top |
| Fixture grid | not modelled; one vise screw sits at x 8.85, y -287.8 |

## 3. Loads

Forces come from rCAD's own cutting model (Kienzle; `docs/cutting-model.md`): tangential force = power / cutting speed, sideways force on the tool and work = tangential x 1.3 x sqrt(1 + 0.35²). `cutsim/cutsim.py` runs rCAD's stock simulation on plain side cuts with the 1/4 in 3-flute tool: 385 N at 835 W and 9000 rpm, 127 N at 736 W and 24 000 rpm, the same ratios.

| Case | Tangential N | Sideways mean N | Peak N | Axial N | Torque at 46 mm, mean / peak N·m |
|---|---|---|---|---|---|
| Roughing, 850 W at 9000 rpm | 284 | 391 | 782 | 111 | 18.1 / 36.2 |
| Finishing, 450 W at 24 000 rpm | 56 | 78 | 155 | 22 | 3.6 / 7.2 |

- **Dynamic factor 2** on the mean for interrupted cutting (G, argued): a straight-tooth cut 1 mm wide and 6 mm deep peaks at 4.1 times its mean, but the 38 degree helix spreads each tooth over about 85 degrees at 6 mm depth, so successive teeth overlap. Two is on the safe side for depths of 5 mm and more.
- **Worst direction assumed:** the whole sideways force is taken tangential to the work at its largest radius for torque, and square to the axis for bending.
- **For scale:** rCAD's model says the tool itself bends 0.17 mm at the roughing load with 24 mm stick-out. The axis targets (20 and 5 µm) are far tighter than the cutter.

## 4. Concept comparison

| Concept | Lost motion | Torsional stiffness N·m/rad | Holding torque N·m | Cost USD | Precision needed from parts made on this mill |
|---|---|---|---|---|---|
| **Bought strain-wave unit, size 25, 100:1, crossed-roller output (chosen)** | backlash 20 arc-sec or less, hysteresis 1 arc-min (D) | 3.1 to 5.7 x10^4 (D) | 67 rated, 157 repeated peak, 284 momentary (D) | 269 (D) | flat faces; concentricity 0.1 mm (D, vendor manual) |
| Same, size 17 (fits a NEMA 23 directly) | same | 1.0 to 1.6 x10^4 (D) | 24 / 54 / 108 (D) | 245 (D) | same |
| Cycloidal drive machined here | about 5 arc-min with +/- 0.03 mm lobes and pin circle (C from G) | 2 to 5 x10^4 (G) | high | about 60 | +/- 0.005 mm profiles for 1 arc-min: not achievable |
| Two-stage preloaded belt, 25:1 | none in the belts; about 13 arc-sec from the motor (C) | about 1.1 x10^4 (C from Gates belt data) | 50, set by the motor | about 60 | low, but a 127 mm output pulley and no stiffness to spare |
| Split or spring-loaded worm (bought table, motorised) | 0.2 deg stock on a Sherline (D); 1 to 2 arc-min adjusted (G) | about 1 x10^5 (G) | self-locking | 550 (D, Vertex HV-4) | a worm cannot be cut with an end mill; spring loading gives way above its preload |
| Direct drive, 110 mm torque motor | none | servo stiffness only | 5 to 8 continuous (D): too little | 1,766 and up (D) | low, but not a stepper and far over budget |

Why the strain-wave unit: it is the only option that puts all the precision in a bought part and still costs under 300 USD. All four sizes cost within 50 USD of each other, so the stiffest one that fits was taken. The datasheets and STEP are StepperOnline's; ratings match the Laifual and Harmonic Drive tables line for line, and the stiffness values are Harmonic Drive's for the same size (Laifual publishes none for 100:1).

What the reducer alone cannot do: hold 20 µm at the largest radius while roughing (24.6 µm), or carry bending on its own bearing (iteration 1). Hence the separate bearing and the brake.

## 5. The design (iteration 5)

Frame: X along the axis, origin on the axis at the faceplate's front face. Front to back:

| Part | What it does | Made from |
|---|---|---|
| Vise H54-8-55 | workholding; base on the faceplate, jaws pointing along +X, 4 x M6 from behind | bought |
| Faceplate, Ø118 x 12.7 | carries the vise; comes off the nose with six M6 from the front, so the vise can be changed without opening the axis | 6061 plate |
| Spindle nose, Ø110 | bolts to the bearing's inner ring (8 x M5 from the front); captive M6 nuts in open slots for the faceplate | 6061 plate |
| RU85 crossed-roller bearing | the spindle bearing: both rings have their own bolt holes, so there are no fits to hold | bought |
| P1 bearing plate, 31.75 | bearing recess in front, brake recess and friction lining behind | 6061 plate |
| Rotor with brake disc | joins the reducer's output flange to the inner ring; its rim is the brake disc on a 1 mm web | 6061 plate |
| Brake piston + 2 O-rings | ring piston in a groove in plate C, pushes the disc rim onto the lining | 6061 plate, bought O-rings |
| C cylinder plate, 38.1 | spacer round the reducer body, brake cylinder, air port | 6061 plate |
| HSS-25-100 reducer | positions the spindle; its flange is trapped in plate D | bought |
| D motor plate, 31.75 | clamps the reducer, carries the motor; shimmed off plate C | 6061 plate |
| Motor 34E1KIP65-45 | IP65 closed-loop stepper, shaft straight into the wave generator | bought |
| Base plate, 12.7 | joins the plates to the table: 4 x M8 down into the grid, 4 x M8 up into nuts in pockets milled in the plates' faces | 6061 plate |
| Splash ring, sensor bracket | cover the nose gap; hold the home sensor | printed ASA |
| Tailstock plate + base + 12 mm spot drill | dead centre in a pinch clamp; slides on slots, any length | 6061 plate, bought |

How the constraints shaped it:

- **No threads, no small holes.** A 1/4 in end mill cannot make a tap drill hole. Every thread is in a bought part (bearing rings, reducer, vise) or is a nut. Nuts sit in pockets milled into a plate's face, including the nuts that hold the standing plates down on the base: a slot for the bolt and a pocket for the nut, cut from the face, replace a hole drilled into the plate's edge.
- **No fits to hold.** The bearing and the reducer locate on bolted flat faces with clearance round them. They centre themselves when the rotor is bolted up first and the tie bolts last. The only bores that matter are the reducer and motor pilots in plate D, and the vendor allows 0.1 mm there.
- **Two bearings on one spindle.** The reducer has its own output bearing. So that it never fights the main bearing, plate D is bolted to plate C over shims chosen at assembly to fill the measured gap, and the rotor's joints are flat faces that take up any offset before they are tightened.
- **Brake.** The disc is clamped between a ring piston (two O-rings in step glands, 0.3 mm stroke, no sliding fits) and a lining bonded to plate P1. The disc's 1 mm web bends 0.15 mm to close the gap; in its own plane it is about 20 times stiffer than the reducer.
- **Coolant.** IP65 motor; the reducer's input is sealed by its own O-ring against plate D; the plate joints get RTV; the brake cavity is closed; the bearing has its own seals behind a printed splash ring with a drain; the home sensor is a potted M8 inductive type.

## 6. Iteration history

| Iteration | What changed | Why | At the jaws, rough µm | 100 mm out, no tail, µm | Ø25 x 120 with tail, µm | Windup at 46 mm, rough µm (brake on) | First mode Hz, FE / with bearing and joints |
|---|---|---|---|---|---|---|---|
| 1 | Baseline: the reducer's own bearing is the spindle; one 19 mm plate; no brake, no tailstock | simplest thing that could work | 113.4 | 377.3 | n/a | 24.6 (no brake) | n/a / n/a |
| 2 | Separate RU85 main bearing; three bolted plates (31.75 / 38.1 / 31.75) on a base with two rows of bolts; rotor; tailstock | iteration 1 bent 113 µm at the jaws and loaded the reducer's bearing to 89 % of its limit | 11.6 | 49.0 | 15.2 | 24.6 (no brake) | 791 / 581 |
| 3 | Disc brake (piston in plate C, lining on P1), air port, home sensor | the reducer alone winds up 24.6 µm at 46 mm when roughing | 12.2 | 51.1 | 15.5 | 24.6 (0.50) | 740 / 552 |
| 4 | Brake disc on a 1 mm web with a narrower friction ring; shims between C and D; air tube bonded into C; splash ring; home sensor on a printed bracket in front; shorter tail centre; lower tie bolts raised | iteration 3's 2.5 mm disc would yield (478 MPa); the reducer was pinched between two bearings; the sensor did not fit the base; coolant could reach the bearing | n/a | n/a | n/a | n/a (no brake) | n/a / n/a |
| 5 | Table bolts moved; bearing lands under P1 and D, C lifted off the base; pocket corners R3.6; tailstock clamp pockets and slit a 6.35 cutter can make; reducer turned 4.5 degrees | adversarial review: a hex key could not reach two table bolts past the tie bolts; plate edges cut from two sides would rock; three features could not be milled | 13.7 | 54.4 | 13.2 | 24.6 (1.13) | 650 / 495 |

Each iteration is a full rCAD model with every screw (`iter<N>/a-axis.json`), built by `scripts/build.py <N>`, and has its own `calc.md`, `fea/` and renders. Iteration 1 has no modal result: in the coarser modal mesh one of its thin parts came loose (0 Hz modes), and it was not worth chasing for a design already ruled out. The brake-on windup of iterations 4 and 5 is larger than 3's because the disc's web is thinner; it is the price of a disc that does not yield.

## 7. Simulation results, iteration 5

How the numbers are made:

- **Structure (F):** rCAD's FE solver, 10-node tetrahedra, 5 mm mesh (about 500 000 nodes), parts bonded where their faces meet, base plate's underside fixed. A 40 x 40 test bar is gripped in the vise and loaded at its end. The static runs use rCAD's iterative solver, which takes one material for every part (aluminium), so the steel vise is modelled about three times too soft. One run with real materials quantifies that.
- **Bearing (C):** the RU85 as 36 rollers of 6 x 4.8 mm at 45 degrees with Palmgren's line-contact law and 2 µm preload, then halved for ring and seat flexing. Roller count and size are guessed to fit the catalogue's static rating; the halving is a guess.
- **Bolted joints (G):** each joint as a contact layer of 2000 N/mm³. Published values run 500 to 5000, so this column could be half or double.
- **Check of the solver:** a 10 x 10 x 100 cantilever came out at 0.5816 mm against 0.5806 by beam theory, and its first mode at 814 Hz against 816.

### Deflection at the tool point, headstock only (no tailstock)

Load at the end of a 40 x 40 test bar gripped in the vise. 'Structure' is the bonded FE model (F; every part aluminium, so the steel vise is modelled too soft); the bearing (C) and the bolted joints (G: contact stiffness 2000 N/mm3) are added in series. µm per the mean roughing force 391 N, and per the finishing force 78 N.

| Load point | Dir | Structure F | of which the test bar C | Bearing C | Joints G | Total rough µm | Total less the bar, rough µm | Finishing µm (less the bar) |
|---|---|---|---|---|---|---|---|---|
| chuck, 10 mm past the jaws | Fz | 6.6 | 0.1 | 5.5 | 1.8 | 13.8 | 13.7 | 2.73 |
| chuck, 10 mm past the jaws | Fy | 4.7 | 0.1 | 5.5 | 1.5 | 11.7 | 11.7 | 2.31 |
| overhang, 100 mm past the jaws | Fz | 38.5 | 12.1 | 19.2 | 8.9 | 66.6 | 54.4 | 10.81 |
| overhang, 100 mm past the jaws | Fy | 30.8 | 12.1 | 19.2 | 8.1 | 58.1 | 46.0 | 9.13 |

Check with real materials (steel vise, bearing ring and test bar; direct solver, 6 mm mesh): the structure column at 100 mm, vertical, is 23.2 µm instead of 38.5 µm, of which the steel bar itself is 4.2 µm (F). So the all-aluminium numbers above overstate the structure by about 15 µm at that point.

### With and without the tailstock (aluminium bar between the jaws and the centre)

Tailstock stiffness at the centre: 34 N/µm vertical, 42 N/µm sideways (F, bonded); centre-in-hole contact 100 N/µm (G).

| Part | No tailstock, at the free end, rough µm | No tailstock, mid-length µm | With tailstock, mid-length, rough µm | With tailstock, finishing µm |
|---|---|---|---|---|
| Ø25 x 120 mm | 209 | 41.2 | 13.2 | 2.61 |
| Ø40 x 120 mm | 64 | 23.1 | 8.5 | 1.69 |
| Ø16 x 100 mm | 619 | 91.5 | 24.6 | 4.88 |

### Torsion: windup at the work surface

Gear: K1/K2/K3 = 31000/50000/57000 N·m/rad up to 14.0/48.0 N·m (D). Brake: disc 7.4e+05 N·m/rad (C: ring formula, hub share from iteration 3's FE) in series with the lining 1.67e+07 (G) = 7.09e+05 N·m/rad.

| Case | Radius mm | Torque N·m | Gear only: windup mrad | Gear only: µm at the surface | With brake: µm |
|---|---|---|---|---|---|
| roughing mean | 46 | 18.1 | 0.533 | 24.6 | 1.13 |
| roughing mean | 25 | 9.8 | 0.316 | 7.9 | 0.33 |
| roughing peak | 46 | 36.2 | 0.895 | 41.3 | 2.26 |
| roughing peak | 25 | 19.6 | 0.563 | 14.1 | 0.66 |
| finishing mean | 46 | 3.6 | 0.116 | 5.3 | 0.22 |
| finishing mean | 25 | 1.9 | 0.063 | 1.6 | 0.07 |

### Brake

- Piston ring Ø93 to Ø122.4: 4974 mm² (C). At 0.6 MPa (G: shop pressure not confirmed) less 120 N seal drag (G): clamp force 2864 N (C).
- Holding torque at mean radius 58 mm: 37 N·m with µ 0.3, 74 N·m with µ 0.6 (C; µ of the bonded abrasive lining on aluminium is G, 0.6 to 0.8 expected).
- Needed: 18.1 N·m mean, 36.2 N·m peak at the 46 mm radius (C).
- The disc's 1.0 mm web, 16 mm wide, bends 0.15 mm to reach the lining: 121 MPa and 729 N of the clamp force (C); with the gap 0.1 mm larger, 202 MPa and 1215 N. 6061-T6 yields at 276 MPa and takes about 100 MPa for 10^8 cycles.
- Clamp force left on the lining: 2135 N.

### Lost motion, accuracy, repeatability

| Source | arc-sec | µm at 46 mm | µm at 25 mm |
|---|---|---|---|
| Gear backlash (D: at most) | 20.0 | 4.5 | 2.4 |
| Gear hysteresis loss on torque reversal (D) | 60.0 | 13.4 | 7.3 |
| Wave generator on the motor shaft: key bonded with Loctite 638 (C: none) | 0.0 | 0.0 | 0.0 |
| Friction joints rotor / bearing / nose / faceplate / vise (C: none below their slip torque) | 0.0 | 0.0 | 0.0 |
| Closed-loop drive: +/- 1 encoder count (C) | 6.5 | 1.5 | 0.8 |
| Motor step accuracy 5 % of a step (D) | 6.5 | 1.5 | 0.8 |
| **Sum, worst case (reversing direction under load)** | 93 | 20.8 | 11.3 |
| Root-sum-square | 64 | 14.3 | 7.7 |
| Positioning accuracy: gear transmission error (D, cyclic and repeatable) | 60 | 13.4 | 7.3 |
| Repeatability approaching from one side: 2 counts + 5 arc-sec for the gear (G) | 11.5 | 2.6 | 1.4 |

### Motor, speed, inertia

- Rotating group without a workpiece: 1.96 kg, 2712 kg·mm² about the axis (C, from the CAD). A Ø40 x 100 aluminium part adds 68, a Ø90 x 120 part 2087 kg·mm².
- Reflected to the motor through 100:1: 2.78 g·cm² (typical part), 4.80 g·cm² (largest part), against 2213 g·cm² of rotor plus wave generator (D). Inertia ratio 0.0013 to 0.0022.
- Motor torque for 500 °/s² with the largest part: 0.28 N·m (C); for cutting through the gear at 70 % efficiency (G): 51 mN·m finishing, 258 mN·m roughing at 46 mm.
- Motor has 4.5 N·m holding (D) and 3.24 N·m at 600 rpm (D: vendor curve at 60 V; a 48 V supply gives a little less at speed): margin 6.0x over acceleration plus roughing.
- 600 motor rpm = 6 rpm = 36 °/s at the axis. 1111.111 steps per degree with the drive set to 4000 pulses per rev; 40 kHz step rate at top speed.

### Natural frequencies

Tooth passing: 450 Hz at 9000 rpm, 1200 Hz at 24 000 rpm (3 flutes); spindle rotation 150 and 400 Hz.

| Mode | Hz | Source |
|---|---|---|
| Structure mode 1 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 650 | F |
| Structure mode 2 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 816 | F |
| Structure mode 3 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 1234 | F |
| Structure mode 4 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 1482 | F |
| Structure mode 5 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 1889 | F |
| Structure mode 6 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 2019 | F |
| torsion gear no part | 538 | C |
| torsion brake no part | 2628 | C |
| torsion gear Ø40 x 100 | 531 | C |
| torsion brake Ø40 x 100 | 2596 | C |
| torsion gear Ø90 x 120 | 404 | C |
| torsion brake Ø90 x 120 | 1976 | C |
| tilt lumped no part | 1516 | C |
| tilt lumped typ part | 1016 | C |
| Structure mode 1 scaled for the bearing and joint compliance the FE model leaves out | 495 | C from F |

### Bearing

- Roughing at 100 mm past the jaws: moment 70 N·m, equivalent load 2079 N (C) against C = 20.3 kN (D): L10 = 3.32e+06 h at 10 rpm (C).
- Static: peak equivalent 4155 N against C0 = 29.5 kN (D): safety 7.1 (C).

### Bolted joints (slip against the roughing peak torque at 46 mm)

| Joint | Screws | Tightening N·m | Preload each N | Share of proof | Slip torque N·m (µ 0.15) | Margin |
|---|---|---|---|---|---|---|
| vise to faceplate | 4 x M6 12.9 | 12.0 | 10000 | 51 % | 120 | 3.3x |
| faceplate to nose | 6 x M6 8.8 | 8.0 | 6667 | 57 % | 300 | 8.3x |
| nose to bearing inner ring | 8 x M5 12.9 | 7.0 | 7000 | 51 % | 273 | 7.6x |
| rotor to bearing inner ring | 8 x M5 12.9 | 7.0 | 7000 | 51 % | 273 | 7.6x |
| rotor to gear output flange | 8 x M8 12.9 | 30.0 | 18750 | 53 % | 472 | 13.1x |

Vise prying: peak force 100 mm past the jaws puts 113 N·m on the vise base; the far pair of M6 sees 1369 N extra against 10000 N preload (14 %): the joint stays closed (C).

### Thermal growth

A 10 K rise of the aluminium housing (G: motor at holding current) lifts the axis 18 µm and grows 120 mm of stack 28 µm (C).


Reading the results against the targets:

- **At the jaws:** 13.7 µm roughing, 2.73 µm finishing. Both targets are met.
- **100 mm past the jaws with no tailstock:** 54.4 µm roughing. Not met, and it cannot be with a 120 mm bearing: the bearing's tilt alone is about 19 µm there. Use the tailstock, or rough at half the load.
- **Brake stiffness source:** iteration 3's 2.5 mm disc was solved by FE (1.7 x10^6 N·m/rad). The 1 mm web of iterations 4 and 5 would not solve (the solver overflowed on a mesh fine enough for it), so its value is the ring formula with the hub's share taken from that FE result: 7.4 x10^5 N·m/rad (C).
- **Windup:** met with the brake on. On the reducer alone, roughing at the full 46 mm radius is 24.6 µm (over) and finishing is 5.3 µm (at the limit); on a Ø50 part both are met.
- **Resonance:** on the reducer alone the axis's torsional mode is 404 to 538 Hz, which brackets the 450 Hz tooth-passing frequency at 9000 rpm. Continuous 4-axis cutting at 9000 rpm with a heavy cut could excite it; at 24 000 rpm (1200 Hz) it is well clear. With the brake on the mode is above 1976 Hz. The first bending mode of the head carrying a 40 x 40 x 100 steel bar with no tailstock is 650 Hz in the bonded FE model and about 495 Hz once the bearing and joints are counted (C from F): close to 450 Hz, so a heavy bar that long should not be roughed at 9000 rpm without the tailstock.
- **Motor:** six times the torque needed for 500 °/s² plus roughing. Top speed is set by the stepper: 600 rpm is 36 °/s.
- **Thermal:** a 10 K rise lifts the axis 18 µm. Probe the centre height again once the motor is warm, and let the drive cut the current at standstill.

Evidence files: `iter5/fea/results.json`, `fea-chuck-Fz.png`, `fea-overhang-Fz.png`, `fea-tail-Fz.png`, `fea-brake-torsion.png`, `fea-mode1.png` to `fea-mode3.png`, `calc.json`.

## 8. On the machine: interference

rCAD's assembly interference check with the real machine STEP (trunnion left out), the A axis riding with the table, a 6.35 x 24 mm tool under the collet nut. Placement: faceplate front at x 51, axis at y -325.3, z 88.44. `scripts/machine_check.py`, document `iter5/machine/on-machine.json`.

| Position | Axis offsets X / Y / Z | Kind | Result |
|---|---|---|---|
| Y sweep, head right, dy -150.7 | +219.3 / -150.7 / +14.7 | must be clear | clear |
| Y sweep, head left, dy -150.7 | -123.7 / -150.7 / +14.7 | must be clear | clear |
| Y sweep, head right, dy -75.0 | +219.3 / -75.0 / +14.7 | must be clear | clear |
| Y sweep, head left, dy -75.0 | -123.7 / -75.0 / +14.7 | must be clear | clear |
| Y sweep, head right, dy +0.0 | +219.3 / +0.0 / +14.7 | must be clear | clear |
| Y sweep, head left, dy +0.0 | -123.7 / +0.0 / +14.7 | must be clear | clear |
| Y sweep, head right, dy +83.1 | +219.3 / +83.1 / +14.7 | must be clear | clear |
| Y sweep, head left, dy +83.1 | -123.7 / +83.1 / +14.7 | must be clear | clear |
| Z top, tool at X -123.7, -46 mm from the axis | -123.7 / +61.7 / +14.7 | must be clear | clear |
| Z top, tool at X -123.7, +0 mm from the axis | -123.7 / +15.2 / +14.7 | must be clear | clear |
| Z top, tool at X -123.7, +46 mm from the axis | -123.7 / -31.3 / +14.7 | must be clear | clear |
| Z top, tool at X -60.0, -46 mm from the axis | -60.0 / +61.7 / +14.7 | keep-out: the plates stand higher than the retracted tool | A: D motor plate x tool 6.35 x 24 (497 mm³) |
| Z top, tool at X -60.0, +0 mm from the axis | -60.0 / +15.2 / +14.7 | keep-out: the plates stand higher than the retracted tool | A: D motor plate x tool 6.35 x 24 (434 mm³) |
| Z top, tool at X -60.0, +46 mm from the axis | -60.0 / -31.3 / +14.7 | keep-out: the plates stand higher than the retracted tool | A: D motor plate x tool 6.35 x 24 (497 mm³) |
| Z top, tool at X +0.0, -46 mm from the axis | +0.0 / +61.7 / +14.7 | keep-out: the plates stand higher than the retracted tool | A: P1 bearing plate x tool 6.35 x 24 (497 mm³) |
| Z top, tool at X +0.0, +0 mm from the axis | +0.0 / +15.2 / +14.7 | keep-out: the plates stand higher than the retracted tool | A: P1 bearing plate x tool 6.35 x 24 (217 mm³) |
| Z top, tool at X +0.0, +46 mm from the axis | +0.0 / -31.3 / +14.7 | keep-out: the plates stand higher than the retracted tool | A: P1 bearing plate x tool 6.35 x 24 (497 mm³) |
| Z top, tool at X +45.0, -46 mm from the axis | +45.0 / +61.7 / +14.7 | keep-out: the faceplate stands higher than the retracted tool | clear |
| Z top, tool at X +45.0, +0 mm from the axis | +45.0 / +15.2 / +14.7 | keep-out: the faceplate stands higher than the retracted tool | A: Faceplate x tool 6.35 x 24 (120 mm³) |
| Z top, tool at X +45.0, +46 mm from the axis | +45.0 / -31.3 / +14.7 | keep-out: the faceplate stands higher than the retracted tool | clear |
| Z top, tool at X +73.0, -46 mm from the axis | +73.0 / +61.7 / +14.7 | must be clear | clear |
| Z top, tool at X +73.0, +0 mm from the axis | +73.0 / +15.2 / +14.7 | must be clear | clear |
| Z top, tool at X +73.0, +46 mm from the axis | +73.0 / -31.3 / +14.7 | must be clear | clear |
| Z top, tool at X +120.0, -46 mm from the axis | +120.0 / +61.7 / +14.7 | must be clear | clear |
| Z top, tool at X +120.0, +0 mm from the axis | +120.0 / +15.2 / +14.7 | must be clear | clear |
| Z top, tool at X +120.0, +46 mm from the axis | +120.0 / -31.3 / +14.7 | must be clear | clear |
| Z top, tool at X +219.3, -46 mm from the axis | +219.3 / +61.7 / +14.7 | must be clear | clear |
| Z top, tool at X +219.3, +0 mm from the axis | +219.3 / +15.2 / +14.7 | must be clear | clear |
| Z top, tool at X +219.3, +46 mm from the axis | +219.3 / -31.3 / +14.7 | must be clear | clear |
| Z top, tool behind the unit over the plates (route to the left end) | +0.0 / -70.8 / +14.7 | must be clear | clear |
| Z top, tool behind the unit, left end | -123.7 / -70.8 / +14.7 | must be clear | clear |
| top of a 90 mm part, 4 mm past the jaws | +99.0 / +15.2 / +11.2 | must be clear | clear |
| top of a 90 mm part, 60 mm past the jaws | +155.0 / +15.2 / +11.2 | must be clear | clear |
| top of a 90 mm part, 8 mm before the centre | +193.0 / +15.2 / +11.2 | must be clear | clear |
| top of a 20 mm part 4 mm past the jaws, A 0 | +99.0 / +15.2 / -23.8 | reach | clear |
| top of a 20 mm part 14 mm past the jaws, A 0 | +109.0 / +15.2 / -23.8 | must be clear | clear |
| beside a 20 mm part at centre height, 14 mm past the jaws, tool in front, A 0 | +109.0 / +28.7 / -37.1 | reach | clear |
| beside a 20 mm part at centre height, 14 mm past the jaws, tool behind, A 0 | +109.0 / +1.8 / -37.1 | reach | clear |
| beside a 20 mm part at centre height, 50 mm past the jaws, tool in front, A 0 | +145.0 / +28.7 / -37.1 | must be clear | clear |
| tool on the jaw tops (facing the jaw step), A 0 | +92.0 / +15.2 / -22.1 | reach | clear |
| top of a 20 mm part 4 mm past the jaws, A 90 | +99.0 / +15.2 / -23.8 | reach | Z 22 x A: Vise XinDian H54-8-55 (489 mm³) |
| top of a 20 mm part 14 mm past the jaws, A 90 | +109.0 / +15.2 / -23.8 | must be clear | clear |
| beside a 20 mm part at centre height, 14 mm past the jaws, tool in front, A 90 | +109.0 / +28.7 / -37.1 | reach | clear |
| beside a 20 mm part at centre height, 14 mm past the jaws, tool behind, A 90 | +109.0 / +1.8 / -37.1 | reach | clear |
| beside a 20 mm part at centre height, 50 mm past the jaws, tool in front, A 90 | +145.0 / +28.7 / -37.1 | must be clear | clear |
| tool on the jaw tops (facing the jaw step), A 90 | +92.0 / +15.2 / +5.9 | reach | clear |
| Z fully down over the headstock (expected to collide) | -9.0 / +15.2 / -135.3 | expected collision | table 20 x tool 6.35 x 24 (760 mm³); Z 21 x A: Base plate (279 mm³); Z 21 x A: D motor plate (11383 mm³) |

What it shows:

- **The unit against the frame:** clear over the whole Y travel, head parked at either end.
- **Retracted tool:** clear over the motor, the vise and the work. It is **not** clear over the plates and the faceplate: they stand higher than a retracted 24 mm tool. That zone (left of the faceplate) is a keep-out with a tool loaded. The collet nut itself clears the plates by 8 mm.
- **Route to the left end with a tool in:** go behind the unit first (table forward), then along X. Clear.
- **Cutting on top of a Ø90 part:** clear from the jaws to the tail centre.
- **Reach down the side:** the collet nut (Ø22) and the spindle body limit it, not the axis. With 24 mm stick-out the tool gets to centre height beside parts up to about Ø66 (C: nut radius 11 against a part of radius r with the tip 3 mm below centre gives r <= 33). Bigger parts are cut within about 20 mm of the top and turned.
- **Close to the jaws:** with the vise upright (A 90) keep the tool 12 mm or more from the jaw face, or the nut meets the vise.

## 9. Making each part on the mill

All machined parts are 6061 plate, cut with the 1/4 in 3-flute end mill (T24) from one or two sides; edges are broken with the chamfer mill. `scripts/dfm.py` slices each part and checks that every cut is reachable by a 6.45 mm circle from one of two opposite faces, no deeper than the flute (19.05 mm) or, stepped, the stick-out (24 mm), and that the part fits the travel.

| Part | Size mm | Setups | Deepest cut mm | Largest tool that would still fit mm | Check |
|---|---|---|---|---|---|
| Faceplate | 118 x 118 x 12.70 | 2 | 6.5 | 6.47 | pass |
| Spindle nose | 110 x 110 x 13.70 | 2 | 8.5 | 6.47 | pass |
| Base plate | 212 x 156 x 12.70 | 2 | 8.5 | 6.47 | pass |
| P1 bearing plate | 156 x 129 x 31.75 | 2 | 16.0 | 6.47 | pass |
| C cylinder plate | 156 x 129 x 38.10 | 2 | 23.0 (stepped, past the flute) | 6.47 | pass |
| D motor plate | 156 x 129 x 31.75 | 2 | 16.0 | 6.47 | pass |
| Rotor with brake disc | 122 x 122 x 18.85 | 2 | 12.7 | 6.47 | pass |
| Brake piston | 122 x 122 x 6.35 | 2 | 3.4 | 13.94 | pass |
| Tailstock plate | 110 x 92 x 25.40 | 2 | 18.6 | 6.47 | pass |
| Tailstock base | 102 x 110 x 19.05 | 2 | 10.6 | 8.98 | pass |

No CAM has been written. Notes for whoever writes it (the CAM guidelines apply):

| Part | Stock | How it is held and cut | What must be right, and how to check it |
|---|---|---|---|
| Base plate | 1/2 in plate, 215 x 160 | Clamp to the fixture plate on a spoil sheet; top side: M8 counterbores and all through holes, outline. Flip: counterbores for the bolts that come up, located from two of the holes | Thickness as supplied. Hole spacing 175 x 125 for the grid |
| P1 bearing plate | 1-1/4 in plate, 160 x 135 | Rear side first: face, brake recess, nut slots, bolt holes, nut pockets, outline to 19 mm deep (this band is the land that stands on the base). Flip onto the faced side, locate from the tie holes: face, bearing recess 15 deep, bore, rest of the outline 0.2 mm under size | Recess depth 15.00 +0.05 (depth mic); recess Ø120.10 +0.05: the bearing drops in by hand. Brake recess 4.45 +/- 0.03 deep. Faces parallel within 0.02 (indicator on the table) |
| C cylinder plate | 1-1/2 in plate | Front: face, piston groove Ø93.00 -0.05 / Ø122.40 +0.05 x 6.00 deep, bore and outline half way. Flip: face, bore and outline to meet, port hole | Groove walls smooth (finish pass, climb, full depth): they seal against the O-rings. Thickness 38.1 as faced: note it, the rotor shim takes up the difference |
| D motor plate | 1-1/4 in plate | Front: face, reducer recess Ø107.4 x 10.0, pilot bore Ø67.03 +0.03 to 12 mm, nut slots. Flip, locate from the tie holes: motor pilot Ø73.10 x 2, rest of the bore, nut pockets, outline land | Reducer pilot: the reducer pushes in by hand. Front and rear pilots concentric within 0.1 mm (the vendor's limit): indicate the front bore after the flip |
| Rotor with brake disc | 1 in plate, 130 x 130 | Rear first: face, Ø54 boss, M5 counterbores, all holes, friction face. Flip onto a spoil plate with a pocket for the boss, bolt through the M8 holes: hub, then the web pocket last in light cuts | Web 1.0 +/- 0.1 (measure through a hole with a ball mic or by depth). Friction faces flat |
| Brake piston | 1/4 in plate | Double-sided tape or tabs on a spoil sheet; rear: the two O-ring steps; flip: 0.6 relief | Step diameters Ø97.4 +0.05 and Ø118.0 -0.05. It must slide in the groove with the O-rings oiled |
| Spindle nose | 5/8 in plate | Rear: face leaving the 2 mm boss, nut slots, holes. Flip: face, M5 counterbores, outline | Boss and front face parallel within 0.02 |
| Faceplate | 1/2 in plate | Rear: face, vise counterbores, holes. Flip: face, M6 counterbores, target pin hole, outline | Both faces milled, parallel within 0.02: it sets how true the vise runs |
| Tailstock plate | 1 in plate | Rear: face, pinch channel and pockets, nut pockets, bore Ø12.02, slit, outline land. Flip: face, rest of outline | Bore: the spot drill slides in and the pinch bolt locks it |
| Tailstock base | 3/4 in plate | Top: slots, holes, outline. Flip: counterbores | none |
| Friction lining | P400 silicon carbide paper | Cut a ring Ø108 to Ø122 with a knife; bond to the P1 recess floor with slow epoxy under a flat weight, after the bearing screws are in | Even, thin glue line; no glue on the grit |
| Splash ring, sensor bracket | ASA | Printed flat, no supports | none |

Fixturing that has to be made first: a spoil plate with an M8 pattern for the rotor and the discs. The big plates are bolted through their own tie holes after a first clamped operation; none is taller than 38 mm on the table.

## 10. Assembly and alignment

Order (every screw can be reached at its step; checked against the model):

1. **P1:** drop the eight M5 nuts into their slots in the brake recess and tape them. Set the RU85 in its recess, eight M5 x 20 from the front, 7 N·m. Then bond the lining over the nuts.
2. **Rotor:** eight M5 x 20 from the rear into the inner ring, 7 N·m, with medium threadlocker. Measure from P1's rear face to the disc rim and to the lining with a depth micrometer: the gap must be 0.10 to 0.25 mm. Correct it with a shim ring between rotor and inner ring.
3. **C:** oil the two O-rings, fit them and the piston in the groove. Put the two M8 nuts in P1's pockets. RTV on the joint, C onto P1, tie bolts in loosely.
4. **D unit:** nuts for the motor in D's slots. Reducer into D's recess, two M5 x 30 from behind. Wave generator onto the motor shaft with its key and Loctite 638 as the vendor's manual says; grease; motor onto D with four M6 x 30.
5. **Join:** offer the D unit up. Eight M8 x 20 through the bearing's bore into the reducer's output flange, 30 N·m, while D is still free to find its place. Measure the gap between C and D at each tie bolt with feeler gauges, fill each with shim washers, then tighten the six M8 x 120 to 20 N·m. Seal the gap with RTV. Bond the air tube into C.
6. **Base:** four M8 x 25 up through the base into the nuts in P1 and D, 12 N·m (the nuts bear on aluminium). Sensor bracket from below.
7. **Front:** splash ring on with RTV, drain down. Nose: eight M5 x 12. Vise onto the faceplate from behind (four M6 x 16), faceplate onto the nose (six M6 x 16, 8 N·m).
8. **On the table:** four M8 x 16 into the grid, finger tight.

Alignment:

1. **Vise on centre.** Grip a ground pin, turn A, indicate the pin near the jaws. Tap the vise until it runs within 0.01 mm, then tighten its screws through the faceplate (take the faceplate off and put it back; it returns on its face, so re-check).
2. **Axis parallel to X, sideways.** Indicator in the spindle against the side of the pin, traverse X over 80 mm. Swing the unit about one table bolt (the holes have 0.5 mm play) until it reads within 0.01, tighten.
3. **Axis parallel to X, up and down.** Same on top of the pin. Correct with shim stock under the front or rear pair of table bolts.
4. **Centre height and Y centre.** Probe the pin's top and both sides: Y centre is the middle, centre height is the top minus half the pin. Store both in rcnc. Repeat warm.
5. **A zero.** Probe a flat held in the vise at two X positions, or the jaw tops; set A0.
6. **Tailstock.** Slide it up, grip the spot drill, bring its point to a centre mark made at the axis (or indicate the cone while turning A with an indicator on the faceplate). Tap the base sideways and shim for height, then tighten.
7. **Brake.** With 6 bar on, a torque wrench on a bar in the vise must not turn the spindle at 45 N·m. Release: the spindle must turn freely by hand through the motor (drive off).

Cables and air: the motor's two 1 m leads leave toward the back; run them and the 6 mm air line in a loop along the table's back edge to the left wall, long enough for the 234 mm of Y travel. The sensor cable runs along the base's front edge and joins them.

## 11. What rcnc needs (not implemented)

| Item | Value |
|---|---|
| Steps per degree | 1111.111 (drive at 4000 pulses per rev x 100 / 360) |
| Top speed | 36 °/s (2160 °/min, 600 motor rpm, 40 kHz steps); the reducer allows 3500 rpm average input |
| Acceleration | 500 °/s² |
| Home | M8 inductive sensor, NPN normally open, 24 V, on a steel pin in the faceplate; seek at 10 °/s, latch at 1 °/s, always from the same side |
| Brake | one output to a 3/2 valve; wait 0.3 s after switching; never step A with the brake on; brake on by default while 3-axis moves cut, off for A moves |
| Positioning | finish every A move in the same direction (overshoot 1 degree and come back) to take out the reducer's 1 arc-min hysteresis |
| Feed | inverse-time or a feed along the surface for moves with A; A is a rotary axis that wraps |
| Keep-out | with the A axis mounted, no travel with a tool in the spindle over the box from the faceplate leftwards and the unit's width, at any Z; the tool-change and tool-setter route goes behind the unit |
| Work offsets | A zero, the axis's Y centre and centre height, from the probing steps above |
| Drive alarm | the CL86T's alarm output to a feed hold: a closed-loop stepper that loses position must stop the program |

## 12. Risks and assumptions

| # | Assumption or risk | Kind | What it changes | How to settle it |
|---|---|---|---|---|
| 1 | RU85 stiffness (0.7 x10^6 N·m/rad tilt) is calculated from guessed roller data and halved | C / G | the bearing column of every deflection | measure: load a bar in the vise with a known weight under an indicator |
| 2 | Bolted joints at 2000 N/mm³ | G | the joints column, 2 to 9 µm | same test |
| 3 | A clone RU85 at 180 USD has the stated P5 runout and preload | G | runout of the vise; stiffness if it has clearance | buy from a seller who states C0 clearance; measure on arrival |
| 4 | Reducer stiffness is Harmonic Drive's table for the same size, not the maker's own | D (proxy) | windup on the reducer alone | measure windup with a lever and indicator |
| 5 | Brake friction 0.3 to 0.6 on bonded abrasive paper, 6 bar available | G | holding torque 37 to 74 N·m against 36 needed at the peak | the 45 N·m torque-wrench test at assembly |
| 6 | Brake gap 0.10 to 0.25 mm after stacking six tolerances | C | web stress 80 to 200 MPa, clamp force | measured and shimmed at step 2 |
| 7 | The 1 mm web survives the cut and the duty | C | brake | 6061 at 120 MPa nominal; cut it last, supported; inspect after the first 1000 cycles |
| 8 | Brake piston returns by O-ring roll-back and the disc's spring | G | drag when released | check free rotation; add three light springs under the piston if it drags |
| 9 | The vise is drawn from two vendor drawings; thread depths, mass (1.09 kg listed) and the Ø9 bores are not confirmed | D / G | screw lengths, inertia | measure the vise in hand before cutting the faceplate |
| 10 | Left tub wall 72 mm from the table as in the CAD; motor rear ends 4 mm short of it | D (CAD) | whether it fits at all | measure the machine |
| 11 | Fixture grid phase unknown; table holes assumed M8 tapped through | G | exact position (+/- 12.5 mm) | measure one hole |
| 12 | Torsional mode of the axis on the reducer alone sits near 450 Hz | C | chatter in continuous 4-axis roughing at 9000 rpm | rough indexed with the brake; pick the spindle speed away from it; rcnc's listening guard |
| 13 | Headstock taller than a retracted tool | F | a crash if a loaded tool crosses it | rcnc keep-out and route (section 11) |
| 14 | Tailstock is a dead centre in a pinch clamp, 10 to 13 N/µm, and it forces the part's end to where the point is | F | accuracy of long parts | make the centre mark with the part running on the axis; a bought live-centre tailstock on a riser is the upgrade |
| 15 | FE model bonds every joint and fixes the whole base underside | F | structure column is a lower bound | joints added by hand; see 1 and 2 |
| 16 | Home sensor sees a steel pin flush in an aluminium face at 1 mm | G | false triggers from chips | home with the coolant off; rcnc checks the pulse width |
| 17 | Splash ring and bearing seals are enough against low-pressure flood | G | bearing life | drain at the bottom; grease the bearing through its ports yearly |
| 18 | Motor price is a reseller's; StepperOnline's own page would not load. Plate prices are a shop's formula for small cut pieces | D / est | BOM total | check at order time |
| 19 | Prices and lead time: the reducer is listed as pre-order | D | schedule | ask the vendor |

## 13. Files

| Path | What |
|---|---|
| `REPORT.md`, `bom.csv` | this report; every part with vendor, number, quantity, price, link |
| `iter5/a-axis.json` | the final rCAD document: one Part Studio per part, the assembly with every screw, the exploded view |
| `iter5/a-axis-assembly.step`, `step/` | assembly STEP and one STEP per custom part |
| `iter5/machine/on-machine.json` | the assembly on the real machine model |
| `renders/` | whole assembly, exploded, section, on the mill, each custom part |
| `iter1` to `iter5` | every iteration: model, STEP, renders, `calc.md`, `fea/`, `dfm.json` |
| `scripts/` | `build.py` (model), `fea.py` (FE runs), `calc.py` + `phys.py` (hand calculations), `machine_check.py` (interference on the mill), `dfm.py` (can it be milled), `extras.py` (section, exploded), `bom.py`, `report.py`. Rerun in that order |
| `cutsim/` | rCAD cutting simulation behind the load cases |
| `vendor/` | vendor STEP, datasheets and the vise drawings, each with `sources.txt` |

Vendor CAD used: reducer and motor from StepperOnline's file host, RU85 from THK's CAD (via a public repository, THK's terms attached). Drawn by hand from published dimensions because no CAD exists: the vise, the M8 sensor, the spot drill, O-rings and small hardware. Screws, nuts, washers and the dowel pin are rCAD's ISO parts.

rCAD itself got one small change, on the same branch as its own commit: `solve_simulation` takes `probes`, points at which to report the displacement.
