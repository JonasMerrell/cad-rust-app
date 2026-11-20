# Iteration 6: calculated results

Tags: D datasheet, C calculated, F finite elements (rCAD), G guess.

## Loads

| Case | Tangential N | Sideways mean N | Peak N (x2) | Axial N | Torque at 46 mm N·m (mean / peak) |
|---|---|---|---|---|---|
| Roughing 850 W, 9000 rpm | 284 | 391 | 782 | 111 | 18.1 / 36.2 |
| Never-exceed 950 W, 9000 rpm | 317 | 437 | 875 | 124 | 20.2 / 40.4 |
| Finishing 450 W, 24 000 rpm | 56 | 78 | 155 | 22 | 3.6 / 7.2 |

## Deflection at the tool point, headstock only (no tailstock)

Load at the end of a 40 x 40 test bar gripped in the vise. 'Structure' is the bonded FE model (F; every part aluminium, so the steel vise is modelled too soft); the bearing (C) and the bolted joints (G: contact stiffness 2000 N/mm3) are added in series. µm per the mean roughing force 391 N, and per the finishing force 78 N.

| Load point | Dir | Structure F | of which the test bar C | Bearing C | Joints G | Total rough µm | Total less the bar, rough µm | Finishing µm (less the bar) |
|---|---|---|---|---|---|---|---|---|
| chuck, 10 mm past the jaws | Fz | 2.1 | 0.1 | 8.6 | 3.8 | 14.5 | 14.4 | 2.87 |
| chuck, 10 mm past the jaws | Fy | 1.5 | 0.1 | 8.6 | 3.4 | 13.5 | 13.5 | 2.67 |
| overhang, 100 mm past the jaws | Fz | 22.7 | 12.1 | 33.2 | 17.7 | 73.6 | 61.4 | 12.19 |
| overhang, 100 mm past the jaws | Fy | 18.5 | 12.1 | 33.2 | 16.6 | 68.3 | 56.1 | 11.14 |

## With and without the tailstock (aluminium bar between the jaws and the centre)

Tailstock stiffness at the centre: 32 N/µm vertical, 43 N/µm sideways (F, bonded); centre-in-hole contact 100 N/µm (G).

| Part | No tailstock, at the free end, rough µm | No tailstock, mid-length µm | With tailstock, mid-length, rough µm | With tailstock, finishing µm |
|---|---|---|---|---|
| Ø25 x 120 mm | 214 | 43.1 | 13.6 | 2.71 |
| Ø40 x 120 mm | 70 | 25.0 | 8.9 | 1.77 |
| Ø16 x 100 mm | 623 | 93.1 | 25.1 | 4.98 |

## Torsion: windup at the work surface

Gear: K1/K2/K3 = 31000/50000/57000 N·m/rad up to 14.0/48.0 N·m (D). Brake: disc 4.85e+05 N·m/rad (C: ring formula, hub share from iteration 3's FE) in series with the lining 8.87e+06 (G) = 4.6e+05 N·m/rad.

| Case | Radius mm | Torque N·m | Gear only: windup mrad | Gear only: µm at the surface | With brake: µm |
|---|---|---|---|---|---|
| roughing mean | 46 | 18.1 | 0.533 | 24.6 | 1.70 |
| roughing mean | 25 | 9.8 | 0.316 | 7.9 | 0.50 |
| roughing peak | 46 | 36.2 | 0.895 | 41.3 | 3.40 |
| roughing peak | 25 | 19.6 | 0.563 | 14.1 | 1.00 |
| finishing mean | 46 | 3.6 | 0.116 | 5.3 | 0.34 |
| finishing mean | 25 | 1.9 | 0.063 | 1.6 | 0.10 |

## Brake

- Piston ring Ø68 to Ø105.2: 5060 mm² (C). At 0.6 MPa (G: shop pressure not confirmed) less 120 N seal drag (G): clamp force 2916 N (C).
- Holding torque at mean radius 49 mm: 31 N·m with µ 0.3, 62 N·m with µ 0.6 (C; µ of the bonded abrasive lining on aluminium is G, 0.6 to 0.8 expected).
- Needed: 18.1 N·m mean, 36.2 N·m peak at the 46 mm radius (C).
- The disc's 1.0 mm web, 14.5 mm wide, bends 0.15 mm to reach the lining: 147 MPa and 804 N of the clamp force (C); with the gap 0.1 mm larger, 246 MPa and 1340 N. 6061-T6 yields at 276 MPa and takes about 100 MPa for 10^8 cycles.
- Clamp force left on the lining: 2112 N.

## Lost motion, accuracy, repeatability

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

## Motor, speed, inertia

- Rotating group without a workpiece: 2.63 kg, 2813 kg·mm² about the axis (C, from the CAD). A Ø40 x 100 aluminium part adds 68, a Ø90 x 120 part 2087 kg·mm².
- Reflected to the motor through 100:1: 2.88 g·cm² (typical part), 4.90 g·cm² (largest part), against 2213 g·cm² of rotor plus wave generator (D). Inertia ratio 0.0013 to 0.0022.
- Motor torque for 500 °/s² with the largest part: 0.28 N·m (C); for cutting through the gear at 70 % efficiency (G): 51 mN·m finishing, 258 mN·m roughing at 46 mm.
- Motor has 4.5 N·m holding (D) and 3.24 N·m at 600 rpm (D: vendor curve at 60 V; a 48 V supply gives a little less at speed): margin 6.0x over acceleration plus roughing.
- 600 motor rpm = 6 rpm = 36 °/s at the axis. 1111.111 steps per degree with the drive set to 4000 pulses per rev; 40 kHz step rate at top speed.

## Natural frequencies

Tooth passing: 450 Hz at 9000 rpm, 1200 Hz at 24 000 rpm (3 flutes); spindle rotation 150 and 400 Hz.

| Mode | Hz | Source |
|---|---|---|
| Axis turning on the reducer's spring, no part | 528 | C |
| Axis turning against the brake, no part | 2103 | C |
| Axis turning on the reducer's spring, Ø40 x 100 | 522 | C |
| Axis turning against the brake, Ø40 x 100 | 2078 | C |
| Axis turning on the reducer's spring, Ø90 x 120 | 400 | C |
| Axis turning against the brake, Ø90 x 120 | 1593 | C |
| Rotating group tilting on the bearing and joints, no part | 1123 | C |
| Rotating group tilting on the bearing and joints, Ø40 x 100 part | 762 | C |

## Bearing

- Roughing at 100 mm past the jaws: moment 68 N·m, equivalent load 2501 N (C) against C = 17.5 kN (D): L10 = 1.09e+06 h at 10 rpm (C).
- Static: peak equivalent 5000 N against C0 = 22.3 kN (D): safety 4.5 (C).

## Bolted joints (slip against the roughing peak torque at 46 mm)

| Joint | Screws | Tightening N·m | Preload each N | Share of proof | Slip torque N·m (µ 0.15) | Margin |
|---|---|---|---|---|---|---|
| vise to faceplate | 4 x M6 12.9 | 12.0 | 10000 | 51 % | 120 | 3.3x |
| faceplate to nose | 6 x M6 8.8 | 8.0 | 6667 | 57 % | 252 | 7.0x |
| nose and hub clamped on the bearing's inner ring (two faces, the weaker one counted) | 4 x M6 12.9 | 12.0 | 10000 | 51 % | 141 | 3.9x |
| hubs and brake disc to the gear output flange | 8 x M8 12.9 | 30.0 | 18750 | 53 % | 472 | 13.1x |

Vise prying: peak force 100 mm past the jaws puts 113 N·m on the vise base; the far pair of M6 sees 1369 N extra against 10000 N preload (14 %): the joint stays closed (C).

## Thermal growth

A 10 K rise of the aluminium housing (G: motor at holding current) lifts the axis 15 µm and grows 120 mm of stack 28 µm (C).
