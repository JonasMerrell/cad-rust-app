# Iteration 1: calculated results

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
| chuck, 10 mm past the jaws | Fz | 69.7 | 0.1 | 14.0 | 29.8 | 113.5 | 113.4 | 22.52 |
| chuck, 10 mm past the jaws | Fy | 20.6 | 0.1 | 14.0 | 3.3 | 37.9 | 37.8 | 7.51 |
| overhang, 100 mm past the jaws | Fz | 230.1 | 12.1 | 52.5 | 106.7 | 389.4 | 377.3 | 74.90 |
| overhang, 100 mm past the jaws | Fy | 75.1 | 12.1 | 52.5 | 14.8 | 142.4 | 130.3 | 25.87 |

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

- Rotating group without a workpiece: 1.74 kg, 2052 kg·mm² about the axis (C, from the CAD). A Ø40 x 100 aluminium part adds 68, a Ø90 x 120 part 2087 kg·mm².
- Reflected to the motor through 100:1: 2.12 g·cm² (typical part), 4.14 g·cm² (largest part), against 2213 g·cm² of rotor plus wave generator (D). Inertia ratio 0.0010 to 0.0019.
- Motor torque for 500 °/s² with the largest part: 0.28 N·m (C); for cutting through the gear at 70 % efficiency (G): 51 mN·m finishing, 258 mN·m roughing at 46 mm.
- Motor has 4.5 N·m holding (D) and 3.24 N·m at 600 rpm (D: vendor curve at 60 V; a 48 V supply gives a little less at speed): margin 6.0x over acceleration plus roughing.
- 600 motor rpm = 6 rpm = 36 °/s at the axis. 1111.111 steps per degree with the drive set to 4000 pulses per rev; 40 kHz step rate at top speed.

## Natural frequencies

Tooth passing: 450 Hz at 9000 rpm, 1200 Hz at 24 000 rpm (3 flutes); spindle rotation 150 and 400 Hz.

| Mode | Hz | Source |
|---|---|---|
| Axis turning on the reducer's spring, no part | 619 | C |
| Axis turning on the reducer's spring, Ø40 x 100 | 609 | C |
| Axis turning on the reducer's spring, Ø90 x 120 | 436 | C |
| Rotating group tilting on the bearing and joints, no part | 1010 | C |
| Rotating group tilting on the bearing and joints, Ø40 x 100 part | 668 | C |

## Bearing

- The gear unit's bearing sees 139 N·m at the roughing peak, 100 mm past the jaws; its allowable moment is 156.0 N·m (D): 89 %.

## Bolted joints (slip against the roughing peak torque at 46 mm)

| Joint | Screws | Tightening N·m | Preload each N | Share of proof | Slip torque N·m (µ 0.15) | Margin |
|---|---|---|---|---|---|---|
| vise to faceplate | 4 x M6 12.9 | 12.0 | 10000 | 51 % | 120 | 3.3x |
| faceplate to nose | 6 x M6 8.8 | 8.0 | 6667 | 57 % | 300 | 8.3x |
| nose to gear output flange | 8 x M8 12.9 | 30.0 | 18750 | 53 % | 472 | 13.1x |

Vise prying: peak force 100 mm past the jaws puts 113 N·m on the vise base; the far pair of M6 sees 1369 N extra against 10000 N preload (14 %): the joint stays closed (C).

## Thermal growth

A 10 K rise of the aluminium housing (G: motor at holding current) lifts the axis 18 µm and grows 120 mm of stack 28 µm (C).
