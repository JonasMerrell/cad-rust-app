# Iteration 2: calculated results

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
| chuck, 10 mm past the jaws | Fz | 4.5 | 0.1 | 5.5 | 1.8 | 11.7 | 11.6 | 2.31 |
| chuck, 10 mm past the jaws | Fy | 3.3 | 0.1 | 5.5 | 1.5 | 10.3 | 10.2 | 2.03 |
| overhang, 100 mm past the jaws | Fz | 33.1 | 12.1 | 19.2 | 8.9 | 61.2 | 49.0 | 9.73 |
| overhang, 100 mm past the jaws | Fy | 26.2 | 12.1 | 19.2 | 8.1 | 53.6 | 41.5 | 8.23 |

## With and without the tailstock (aluminium bar between the jaws and the centre)

Tailstock stiffness at the centre: 10 N/µm vertical, 12 N/µm sideways (F, bonded); centre-in-hole contact 100 N/µm (G).

| Part | No tailstock, at the free end, rough µm | No tailstock, mid-length µm | With tailstock, mid-length, rough µm | With tailstock, finishing µm |
|---|---|---|---|---|
| Ø25 x 120 mm | 205 | 38.6 | 15.2 | 3.01 |
| Ø40 x 120 mm | 60 | 20.5 | 11.2 | 2.22 |
| Ø16 x 100 mm | 616 | 89.1 | 26.3 | 5.23 |

## Torsion: windup at the work surface

Gear: K1/K2/K3 = 31000/50000/57000 N·m/rad up to 14.0/48.0 N·m (D). No brake in this iteration.

| Case | Radius mm | Torque N·m | Gear only: windup mrad | Gear only: µm at the surface | 
|---|---|---|---|---|
| roughing mean | 46 | 18.1 | 0.533 | 24.6 |
| roughing mean | 25 | 9.8 | 0.316 | 7.9 |
| roughing peak | 46 | 36.2 | 0.895 | 41.3 |
| roughing peak | 25 | 19.6 | 0.563 | 14.1 |
| finishing mean | 46 | 3.6 | 0.116 | 5.3 |
| finishing mean | 25 | 1.9 | 0.063 | 1.6 |

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

- Rotating group without a workpiece: 1.93 kg, 2630 kg·mm² about the axis (C, from the CAD). A Ø40 x 100 aluminium part adds 68, a Ø90 x 120 part 2087 kg·mm².
- Reflected to the motor through 100:1: 2.70 g·cm² (typical part), 4.72 g·cm² (largest part), against 2213 g·cm² of rotor plus wave generator (D). Inertia ratio 0.0012 to 0.0021.
- Motor torque for 500 °/s² with the largest part: 0.28 N·m (C); for cutting through the gear at 70 % efficiency (G): 51 mN·m finishing, 258 mN·m roughing at 46 mm.
- Motor has 4.5 N·m holding (D) and 3.24 N·m at 600 rpm (D: vendor curve at 60 V; a 48 V supply gives a little less at speed): margin 6.0x over acceleration plus roughing.
- 600 motor rpm = 6 rpm = 36 °/s at the axis. 1111.111 steps per degree with the drive set to 4000 pulses per rev; 40 kHz step rate at top speed.

## Natural frequencies

Tooth passing: 450 Hz at 9000 rpm, 1200 Hz at 24 000 rpm (3 flutes); spindle rotation 150 and 400 Hz.

| Mode | Hz | Source |
|---|---|---|
| Structure mode 1 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 791 | F |
| Structure mode 2 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 1012 | F |
| Structure mode 3 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 1419 | F |
| Structure mode 4 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 1768 | F |
| Structure mode 5 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 2318 | F |
| Structure mode 6 (bonded FE model with a 40 x 40 x 100 steel bar in the vise) | 2428 | F |
| torsion gear no part | 546 | C |
| torsion gear Ø40 x 100 | 540 | C |
| torsion gear Ø90 x 120 | 408 | C |
| tilt lumped no part | 1525 | C |
| tilt lumped typ part | 1019 | C |
| Structure mode 1 scaled for the bearing and joint compliance the FE model leaves out | 581 | C from F |

## Bearing

- Roughing at 100 mm past the jaws: moment 70 N·m, equivalent load 2079 N (C) against C = 20.3 kN (D): L10 = 3.32e+06 h at 10 rpm (C).
- Static: peak equivalent 4155 N against C0 = 29.5 kN (D): safety 7.1 (C).

## Bolted joints (slip against the roughing peak torque at 46 mm)

| Joint | Screws | Tightening N·m | Preload each N | Share of proof | Slip torque N·m (µ 0.15) | Margin |
|---|---|---|---|---|---|---|
| vise to faceplate | 4 x M6 12.9 | 12.0 | 10000 | 51 % | 120 | 3.3x |
| faceplate to nose | 6 x M6 8.8 | 8.0 | 6667 | 57 % | 300 | 8.3x |
| nose to bearing inner ring | 8 x M5 12.9 | 7.0 | 7000 | 51 % | 273 | 7.6x |
| rotor to bearing inner ring | 8 x M5 12.9 | 7.0 | 7000 | 51 % | 273 | 7.6x |
| rotor to gear output flange | 8 x M8 12.9 | 30.0 | 18750 | 53 % | 472 | 13.1x |

Vise prying: peak force 100 mm past the jaws puts 113 N·m on the vise base; the far pair of M6 sees 1369 N extra against 10000 N preload (14 %): the joint stays closed (C).

## Thermal growth

A 10 K rise of the aluminium housing (G: motor at holding current) lifts the axis 18 µm and grows 120 mm of stack 28 µm (C).
