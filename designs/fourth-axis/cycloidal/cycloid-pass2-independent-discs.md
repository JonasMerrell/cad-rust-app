# Cycloidal reducer: geometry and tolerance study

- 24 ring pins of Ø8 on a Ø88 circle, one lobe fewer on the disc: ratio 23:1. Eccentricity 1.25 mm (shortening 0.68).
- Disc outline between R38.75 and R41.25. Smallest concave radius 4.29 mm against the cutter's 3.175: cuttable.
- Pitch lever e x N = 30.0 mm: 0.03 mm of play at the mesh is 3.4 arc-min at the output.

| Case | Clearance cut in (mm) | Builds that bind | Lost motion, one disc, no preload: mean / 95 % worst (arc-min) | Transmission error p-p: mean / 95 % (arc-min) | of which slow (1 to 3 per output turn) / per input turn / the rest (pin passing: what a coarse table cannot remove) | Play left with two discs twisted and locked: mean / 95 % (arc-min) |
|---|---|---|---|---|---|---|
| Perfect parts, no clearance | 0 | True | 0.00 / 0.01 | 0.00 | 0.00 / 0.00 / 0.00 | 0.00 |
| Perfect parts, clearance 0.06 mm | 0.06 | False | 32.24 / 34.84 | 6.24 | 0.10 / 0.16 / 6.40 | 10.21 |
| as milled, +/- 0.03 mm, 60 builds | 0.09 | 2 % | 41.7 / 59.9 | 14.8 / 19.6 | 0.8 / 7.8 / 11.0 | 21.5 / 29.3 |
| careful, +/- 0.015 mm, 60 builds | 0.045 | 2 % | 20.9 / 30.0 | 7.7 / 9.3 | 0.4 / 3.9 / 5.8 | 11.0 / 14.8 |
| probed and corrected, +/- 0.008 mm, 60 builds | 0.024 | 2 % | 10.9 / 16.3 | 4.3 / 5.7 | 0.2 / 2.1 / 3.2 | 6.1 / 8.1 |
| best case, +/- 0.004 mm, 60 builds | 0.012 | 0 % | 4.6 / 7.4 | 2.2 / 2.9 | 0.1 / 1.1 / 1.8 | 3.2 / 4.3 |

Estimates at 36 N·m (C, G): mesh force 1200 N, pin load 522 N, contact stress 525 MPa, torsional stiffness 5.19e+04 N·m/rad (ring mesh 2.68e+05, output pins 2.26e+05, eccentric bearing 9e+04), eccentric bearing load 1252 N.