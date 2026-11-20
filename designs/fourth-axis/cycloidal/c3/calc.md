# Cycloidal iteration c3: calculated results

Tags: D datasheet, C calculated, F finite elements (rCAD), M Monte Carlo tolerance study, G guess.

## Drive train

- Cycloid 23:1 (24 ring pins Ø8 on Ø88, eccentricity 1.25 mm), belt 20:60 = 3:1, total 69:1 (C).
- Motor 23E1KIP65-20: 2.0 N·m holding, 4000 counts per turn (D). One count = 4.7 arc-sec at the output (C).
- Cutting torque at the 46 mm radius: roughing 18.1 mean / 36.2 peak, finishing 3.6 / 7.2 N·m (C, as for the other iterations).

## Torsional stiffness (one disc carrying the torque)

| Element | N·m/rad | Tag |
|---|---|---|
| Ring mesh: 3 pins sharing, steel on steel line contact | 2.68e+05 | C |
| Output sleeves in their holes, sleeve bending | 3.14e+05 | C |
| Eccentric: 6904 (1e+08 N/m, G), Ø12 shaft over a 47 mm span (2.60e+08 N/m, C), two 6001 | 3.55e+04 | C/G |
| **Reducer, one disc** | **2.85e+04** | C |
| Belt (both spans, 9 mm GT, at the output) | 2.86e+05 | G |
| **Drive, motor shaft held** | **2.59e+04** | C |
| Brake (iteration 6: same disc and lining) | 4.6e+05 | C |

For comparison the bought HSS-25 strain-wave unit: 3.1e4 to 5.7e4 N·m/rad (D). With the sprung preload the second disc adds only its blades' stiffness, so this figure holds in both directions up to the preload torque.

| Case | Radius mm | Torque N·m | Windup, drive only: arc-min | µm at the surface | With the brake on: µm |
|---|---|---|---|---|---|
| roughing mean | 46 | 18.1 | 2.39 | 32.2 | 1.72 |
| roughing peak | 46 | 36.2 | 4.79 | 64.4 | 3.44 |
| roughing mean | 25 | 9.8 | 1.30 | 9.4 | 0.50 |
| finishing mean | 46 | 3.6 | 0.48 | 6.4 | 0.34 |
| finishing mean | 25 | 1.9 | 0.26 | 1.9 | 0.10 |

Contact stress at the roughing peak (36 N·m on one disc): ring pins 526 MPa, output sleeves 401 MPa (C). Annealed O1 or 1018 takes about 1000 MPa in line contact before it dents (G: 2.5 to 3 times yield).

## Eccentric and shaft bearings

- Load on a disc's 6904 bearing: 783 N at the mean roughing torque, 1567 N at its peak (C, 1.3 x mesh force). Static safety 2.4 (C0 3700 N, D).
- Life at 10 rpm of the table, roughing all the time: 39,532 h (C). Not a limit.
- Shaft: the mean roughing load bends the Ø12 shaft 3.0 µm at the eccentric (C); front 6001 carries 632 N.

## Motor

| Case | Output N·m | At the motor N·m | Margin at 300 rpm | at 600 rpm | at 900 rpm |
|---|---|---|---|---|---|
| roughing mean | 18.1 | 0.45 | 2.7 | 2.4 | 1.7 |
| roughing peak | 36.2 | 0.84 | 1.4 | 1.3 | 0.9 |
| finishing mean | 3.6 | 0.13 | 9.3 | 8.4 | 6.0 |

- Efficiency 0.70 for the reducer (G) and 0.95 for the belt (G); the 6 N·m preload between the discs costs 0.05 N·m at the motor (C).
- Table speed: 26 / 52 / 78 deg/s at 300 / 600 / 900 motor rpm (C). Accelerating at 500 deg/s² takes 0.033 N·m (C).
- Back-drivable: efficiency backwards about 0.57 (C from the forward guess), so the motor must hold 0.30 N·m against the roughing peak; it has 2.0 (D). With air and power off the table can be turned by hand with a few N·m.

## Torsional natural frequency of the table on the drive

| Work | One disc Hz | Both discs (below preload) Hz | Brake on Hz |
|---|---|---|---|
| no part | 405 | 405 | 1752 |
| Ø40 x 100 aluminium | 401 | 401 | 1737 |
| Ø90 x 120 aluminium | 328 | 328 | 1421 |

Tooth passing is 450 Hz at 9000 rpm and 1200 Hz at 24 000 rpm with the three-flute cutter.

## Lost motion and accuracy (tolerance study, 60 builds per row)

| How well the parts are made | Clearance cut in mm | Lost motion, one disc, no preload: mean / 95 % arc-min | Position error p-p: mean / 95 % arc-min | of which slow / per input turn / pin passing | Play left somewhere in the turn with the two discs twisted and locked: mean / 95 % arc-min | The same as µm at 46 mm |
|---|---|---|---|---|---|---|
| as milled, +/- 0.03 mm | 0.075 | 35.6 / 55.3 | 13.9 / 17.9 | 0.7 / 7.9 / 9.9 | 17.8 / 22.9 | 239 / 308 |
| careful, +/- 0.015 mm | 0.045 | 20.5 / 30.6 | 7.9 / 10.4 | 0.4 / 4.0 / 6.1 | 10.6 / 13.4 | 143 / 180 |
| probed and corrected, +/- 0.008 mm | 0.02 | 8.5 / 13.4 | 3.8 / 5.2 | 0.2 / 1.9 / 2.9 | 5.1 / 6.5 | 69 / 87 |
| best case, +/- 0.004 mm | 0.012 | 4.6 / 7.1 | 2.1 / 2.7 | 0.1 / 1.0 / 1.6 | 2.9 / 3.7 | 38 / 50 |

- Belt: a GT-profile belt has about 0.02 mm of tooth clearance (G): 0.10 arc-min at the output. An HTD belt (0.1 mm) would give 0.52; so GT, not HTD.
- One encoder count is 4.7 arc-sec (C).
- The study is rigid-body: every pin is taken as hard and only one pin drives at a time. Real parts give a little and share the load, which smooths the pin-passing part; I have not credited that.

## Sprung preload (carrier 2 spider)

- One blade, load at the sleeve seat: 1273 N/mm (F: 128 000 elements, 1.2 mm); hand estimate 1499 (C). With the blade's twist under the offset load: 908 N/mm (C).
- Between the carriers: 2291 N·m/rad (C). The 6 N·m preload bends the blades 9.0 arc-min (0.076 mm at the sleeves).
- The gap between the discs changes by up to 6.5 arc-min over a turn for probed parts (M, 95 %), so the preload swings 6 +/- 2.2 N·m. It never drops below the finishing torque (3.6 N·m at the 46 mm radius).
- Highest blade stress at the top of the swing: 214 MPa (F). 4140 pre-hardened yields at about 650 MPa, annealed O1 at about 400 (D, typical).
- Torque in the direction disc A carries: full stiffness to any load. In the other direction: both discs up to about the preload, then disc B's blades give until disc A's other flank takes over, so beyond the preload there is lost motion of the size in the table above. Roughing is done with the brake on; finishing stays below the preload.

## Thermal

- Pin plates of steel, discs of steel: a 10 K rise changes the radial clearance at the pins by 0.0 µm (C): the reducer is all steel where it matters.
- Axis height and length grow as iteration 6 (15 µm and about 28 µm per 10 K, C).

## Deflection at the tool point (this iteration's FE model, bonded, every part aluminium)

| Load point | Dir | Structure F µm | Iteration 6 structure µm | Total with bearing and joints, roughing µm | Finishing µm |
|---|---|---|---|---|---|
| chuck | Fz | 2.1 | 2.1 | 14.4 | 2.88 |
| chuck | Fy | 1.5 | 1.5 | 13.4 | 2.68 |
| overhang | Fz | 22.6 | 22.7 | 61.3 | 12.23 |
| overhang | Fy | 18.3 | 18.4 | 56.0 | 11.17 |

Natural frequencies of the bonded model with a 100 mm test bar: 0, 0, 0, 0, 0, 0 Hz (F).

## Brake (unchanged from iteration 6)

Clamp 2916 N at 0.6 MPa, holding 31 to 62 N·m for µ 0.3 to 0.6; needed 18 mean / 36 peak.
