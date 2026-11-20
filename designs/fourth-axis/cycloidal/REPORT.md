# DMC2 rotary A axis with a self-made cycloidal reducer

Design only: nothing has been built, bought or run on the machine. Tags used for numbers: D vendor datasheet, C calculated, F rCAD finite elements, M Monte Carlo tolerance study, G guess. The model is iteration c4 in `c4/`; the bought strain-wave design it started from is in `../REPORT.md` (iterations 1 to 5) and `../iter6/`.

## 1. Result in short

- **What it is**: iteration 6's low headstock (steel faceplate with the XinDian H54-8-55 vise, RU66 crossed-roller main bearing, air brake, 120 mm tall, turned so the headstock stands at the right end of the table and the tool setter stays free) with a 23:1 cycloidal reducer cut on the DMC2 and a NEMA 23 closed-loop stepper beside the housing on a 3:1 GT belt: 69:1 overall.
- **Size**: 211 mm from the jaw tips to the back of the belt guard (iteration 6 with its NEMA 34 in line: 290 mm), 150 mm wide at the housing plus the motor to 148 mm on the front side of the table, 120 mm tall.
- **Backlash**: zero up to the preload torque (6 N·m, swinging +/- 2.2 over a turn), by two discs sprung against each other through flexure blades in the output carrier. Finishing at the largest radius needs 3.6 N·m. Roughing (18 to 36 N·m) is done with the brake on.
- **Position error**: this is the weak point against the bought unit. With parts made to 0.008 mm (probed and corrected) the study gives 3.8 arc-min peak to peak (25 µm either way at the 46 mm radius, 14 µm at 25 mm); as milled (0.03 mm) 14 arc-min. It repeats, so an error map in rcnc can take most of it out; how much is left has to be measured on the real unit (my estimate: about 1 arc-min). The strain-wave unit is 1 arc-min as bought.
- **Stiffness**: drive 2.59e+04 N·m/rad (strain-wave unit 3.1e4 to 5.7e4, D). Windup when finishing at 46 mm radius 6.4 µm on the drive, 0.34 µm with the brake; roughing peak with the brake 3.4 µm (C).
- **Structure**: 14.4 µm at the jaws under the mean roughing force, 2.9 µm when finishing (targets 20 and 5); 61 µm 100 mm out without the tailstock (F, this model; bearing and joints added by hand, C/G).
- **Makeable**: 21 of 21 machined parts pass the one-cutter check (1/4 in end mill, two sides at most, no tapped holes, no hole under 6.5 mm). Steel parts: faceplate, nose, hubs, carriers, discs, pin plates, base.
- **On the machine**: 65 of 66 required poses clear in the real machine model, tool setter touch-offs and routes with the 21 mm tool, the 24 mm tool and the 40 mm probe included; the one flagged pose is the machine model's own base against its own table at the end of Y travel, there without the A axis too (section 8).
- **Model**: 210 placed parts, every screw, nut, pin and bearing; rCAD's interference check finds 0 overlaps.

### Questions for Jonas

1. **Which measuring tools do you have?** Micrometer (ranges), pin gauges, gauge blocks or 1-2-3 blocks, dial test indicator (resolution), ring gauge or calibration ball, surface plate; and has the probe's repeatability been measured? This decides which row of the tolerance table is realistic.
2. Is there shop air for the brake (0.6 MPa assumed)? Without the brake the cycloidal drive should not rough.
3. The tool setter is taken at the back-left table corner (rcnc's X -5 / Y 289). The front-left corner was checked as geometry only, since it is outside the CAD's Y travel.
4. Prices in `bom.csv` are mostly my estimates; StepperOnline's pages refused an automated read today.

## 2. Why a cycloid, and what it costs

Asked for by Jonas (2026-11-20): make the cycloidal gearbox the main design, holding tighter tolerances by careful work, in-situ probing and correcting. A cycloidal reducer suits the mill: every part is a flat plate with holes and one outline, the tightest inside radius of the disc is 4.29 mm against the cutter's 3.175, and it is stiff and compact. What it costs is accuracy: the pitch lever is only e x N = 30 mm, so 0.01 mm of error or play at the pins is 1.15 arc-min at the table. The bought strain-wave unit hides that problem inside a ground part.

## 3. Layout

Front to back (X along the axis, origin at the faceplate's front face):

| Part | From x mm | To x mm | Note |
|---|---|---|---|
| Vise, faceplate (steel), spindle nose (steel) | 44.0 | -22.2 | as iteration 6; four M6 through the main bearing's bore |
| RU66 main bearing in plate P1 (31.75 aluminium) | -22.2 | -54.0 | takes all cutting force; lining of the brake on P1's rear |
| Hub disc A, brake disc, hub spacer (steel) | -37.2 | -72.2 | six M6 x 40 clamp them to carrier 1; the spacer runs in the lip seal |
| C1 piston and seal plate (19.05 aluminium) | -54.0 | -73.0 | ring piston on two O-rings, air from the rear through a channel between two ring pins |
| Carrier 1 (steel) | -72.2 | -84.9 | front shaft bearing 6001, seats of the three A sleeves |
| Pin plate G1 (steel) | -73.0 | -85.7 | 24 holes for the 8 mm ring pins |
| Disc A, disc B on 6904 bearings and eccentric bushes | -86.7 | -102.0 | identical discs, cut stacked, one turned half a turn |
| Ring spacer M (aluminium) | -85.7 | -103.0 | around the discs |
| Pin plate G2 (steel) | -103.0 | -115.7 | bored stacked with G1 |
| Carrier 2 spider (steel) | -103.4 | -112.9 | three stiff arms clamp the A sleeves; three 2 mm blades carry the B sleeves |
| Rear plate D with the motor ear (15.88 aluminium) | -115.7 | -131.6 | rear shaft bearing 6001, windows for setting the preload, motor on slots |
| Pulleys, belt, printed guard | -131.6 | -161.4 | GT3 3 mm, 20 and 60 teeth, 9 mm belt |

- Reducer: 24 ring pins Ø8 on Ø88, 23 lobes, eccentricity 1.25 mm, discs 6.35 mm thick between R38.75 and R41.25, output through six Ø10 hardened sleeves on R29 (each two needle-bearing inner rings on an M6), in holes of Ø12.56; the three holes a disc does not use are Ø14.5.
- Input: a bought Ø12 hardened shaft with two eccentric bushes bonded on (bore and journal milled in one clamping), between a 6001 in carrier 1 and a 6001 in plate D.
- Motor 23E1KIP65-20 (IP65, 2 N·m, 4000 counts) on the +Y side, which on the machine is the front of the table where the spindle cannot reach anyway; its flange sits on a 6.56 mm web of plate D with slots for belt tension.
- Everything that locates is a flat face, a bore or a pocket; every screw has a nut in a milled pocket; four M8 x 120 tie rods clamp the plate stack; P1 and D are bolted down to the steel base from below.

**How the preload works.** Sleeves A sit in pockets of carrier 1 and drive disc A; sleeves B sit in pockets on the spider's blade pads and drive disc B. With the three A screws slack, the spider is turned against carrier 1 (two-pin spanner through windows in D) until disc A bears on one flank of the pins and disc B on the other and the blades are bent by the preload torque; then the A screws are tightened through the same windows. The blades follow the change of the gap round the turn, which a rigid lock cannot.

Pictures: `renders/assembly-iso.png`, `renders/assembly-back.png`, `renders/exploded.png`, `renders/section.png` (lengthwise), `renders/cross-section-disc.png` (through disc A), `renders/on-the-mill-*.png`.

## 4. Tolerance study

`scripts/cycloid.py`: planar rigid-body model, one disc on its eccentric inside the ring of pins, output through three sleeves in oversize holes. Per build it draws: every pin's position, a common error of the pin circle's radius, the disc outline's offset and a two-lobe waviness, the eccentric's throw and its bearing clearance, the holes' and sleeves' positions and sizes, each within the tolerance of the row. Both discs run on the same pins and are cut stacked. The clearance cut into the outline and the holes is the smallest at which fewer than 5 % of builds bind. 60 builds per row, 138 positions over a full table turn each.

| How well the parts are made | Clearance cut in mm | Lost motion, one disc, no preload (mean / 95 %) arc-min | Position error peak to peak (mean / 95 %) arc-min | of which once or twice per table turn / per input turn / pin passing | Change of the gap between the two discs over a turn (mean / 95 %) arc-min |
|---|---|---|---|---|---|
| as milled, +/- 0.03 mm | 0.075 | 35.6 / 55.3 | 13.9 / 17.9 | 0.7 / 7.9 / 9.9 | 17.8 / 22.9 |
| careful, +/- 0.015 mm | 0.045 | 20.5 / 30.6 | 7.9 / 10.4 | 0.4 / 4.0 / 6.1 | 10.6 / 13.4 |
| probed and corrected, +/- 0.008 mm | 0.02 | 8.5 / 13.4 | 3.8 / 5.2 | 0.2 / 1.9 / 2.9 | 5.1 / 6.5 |
| best case, +/- 0.004 mm | 0.012 | 4.6 / 7.1 | 2.1 / 2.7 | 0.1 / 1.0 / 1.6 | 2.9 / 3.7 |

One arc-min is 13.4 µm at the 46 mm radius and 7.3 µm at 25 mm.

What it says:

1. As milled the drive is poor: 36 arc-min of lost motion without preload. My first estimate of 5 arc-min, given before this study, was wrong by a factor of seven.
2. Careful work helps in proportion: every column halves when the error halves. That is Jonas's point, and it holds.
3. Twisting the two discs against each other and locking them does not remove the play all the way round: the gap changes by 5 arc-min over a turn even for probed parts. Hence the sprung preload from c2 on.
4. Most of the position error is ripple as each pin takes over (552 times per table turn), and most of that comes from the clearance itself: perfect parts with 0.06 mm clearance already show 6.2 arc-min. So the clearance is kept as small as the measured parts allow, by choosing the pin diameter after the discs are measured (dowel pins or pin gauges in 0.01 mm steps).
5. Limits of the study: rigid parts and one pin driving at a time. Real parts give and share the load, which smooths the ripple; run-in wears the high spots. Neither is credited. The error distributions are my assumption (G); the real ones come from the machine checks in MAKING.md.

## 5. Iterations

| | What changed | Why | Result |
|---|---|---|---|
| c1 | First full model: 23:1, two discs, six sleeves clamped between two solid carriers (twist and lock), aluminium pin plates, NEMA 23 on a 3:1 belt | starting point | 209 parts, no overlaps after three fixes (seal against carrier 1, disc outline facets); 20 of 21 parts machinable (a 1.5 mm² sliver on plate D by the motor ear) |
| c2 | Carrier 2 becomes a spider with three 2 mm flexure blades for the B sleeves; pin plates in steel; belt profile GT instead of HTD | the study showed a rigid lock leaves up to 6 arc-min of play somewhere in the turn; steel discs in aluminium plates change the clearance 5.2 µm per 10 K; an HTD belt's tooth clearance is 0.5 arc-min at the table | blade 1271 N/mm (F), preload 6 +/- 2.2 N·m, blade stress 214 MPa; spider machinable |
| c3 | Sliver on plate D removed; printed cover with three plugs for D's setting windows | machinability; grease in, chips out | 21 of 21 parts machinable; every required machine pose clear; but the modal run showed the motor nodding on its 6.56 mm web at 175 Hz |
| c4 | Motor flange in a pocket of plate D with walls toward the housing, above and outside, instead of on an open web | the 175 Hz motor mode | 255 Hz with the model's light motor block (about 200 Hz with the real motor): better, still the lowest mode; it does not reach the tool point. A printed wedge between the motor body and the housing's side (4.3 mm gap) would raise it further; not drawn |

Two faults in my own checks were found and fixed on the way, and both changed numbers I had reported: (1) the machine check for the turned unit applied its half turn again at every pose, so every second pose had the unit facing the wrong way; (2) the structure FE runs named no material and rCAD then left one part out of the mesh, which made iteration 6 look softer than it is (16.7 µm at the jaws reported, 14.5 µm correct).

## 6. Calculations and simulation

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

- One blade, load at the sleeve seat: 1271 N/mm (F: 128 000 elements, 1.2 mm); hand estimate 1499 (C). With the blade's twist under the offset load: 907 N/mm (C).
- Between the carriers: 2287 N·m/rad (C). The 6 N·m preload bends the blades 9.0 arc-min (0.076 mm at the sleeves).
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
| overhang | Fz | 22.6 | 22.7 | 61.4 | 12.24 |
| overhang | Fy | 18.3 | 18.4 | 55.9 | 11.16 |

Natural frequencies of the bonded model with a 100 mm test bar (F, 7 mm mesh, each part its own material; brake disc, hub spacer and carrier 1 as one cylinder): 255, 558, 671, 789, 1749 Hz. The first (255 Hz) is the motor nodding on its mount, modelled as a 0.72 kg block; with the real 1.2 kg it is about 198 Hz (C). It does not move the tool point. The modes that do are the test bar bending on the spindle: 671, 789 Hz, as in iteration 5. The run also returns one mode at 0 Hz with no mass in it: a sliver the coarse mesh left unattached.

## Brake (unchanged from iteration 6)

Clamp 2916 N at 0.6 MPa, holding 31 to 62 N·m for µ 0.3 to 0.6; needed 18 mean / 36 peak.


## 7. Machinability

Each machined part sliced and tested: a 6.35 mm cutter (plus 0.1 mm) must reach everything that is not material from one face or the other, no deeper than its reach.

| Part | Size mm | Setups | Deepest cut mm | Largest cutter that would still do it mm | Result |
|---|---|---|---|---|---|
| Faceplate (steel) | 99 x 100 x 9.53 | 2 | 6.5 | 6.59 | ok |
| Spindle nose (steel) | 96 x 96 x 12.70 | 2 | 6.6 | 6.59 | ok |
| Hub disc A (steel) | 60 x 60 x 12.70 | 2 | 7.0 | 6.59 | ok |
| Brake disc | 104 x 104 x 3.17 | 2 | 2.2 | 6.59 | ok |
| Hub spacer (steel) | 60 x 60 x 19.05 | 1 | 19.1 | 6.59 | ok |
| Carrier 1 (steel) | 76 x 76 x 12.71 | 2 | 8.0 | 6.59 | ok |
| Carrier 2 spider (steel) | 74 x 66 x 9.53 | 1 | 9.5 | 6.47 | ok |
| Cycloid disc A | 82 x 82 x 6.35 | 1 | 6.3 | 9.16 | ok |
| Cycloid disc B | 82 x 82 x 6.35 | 1 | 6.3 | 9.16 | ok |
| Eccentric bush A | 20 x 20 x 9.00 | 1 | 9.0 | 11.97 | ok |
| Eccentric bush B | 20 x 20 x 9.00 | 1 | 9.0 | 11.97 | ok |
| P1 bearing plate | 150 x 112 x 31.75 | 2 | 16.0 | 6.47 | ok |
| C1 piston and seal plate | 150 x 112 x 19.05 | 2 | 12.4 | 6.47 | ok |
| G1 pin plate | 150 x 112 x 12.70 | 1 | 12.7 | 6.47 | ok |
| M ring spacer | 150 x 112 x 17.25 | 1 | 17.2 | 6.47 | ok |
| G2 pin plate | 150 x 112 x 12.70 | 1 | 12.7 | 6.47 | ok |
| D rear plate and motor ear | 223 x 112 x 15.88 | 2 | 8.0 | 6.47 | ok |
| Base plate (steel) | 208 x 150 x 7.94 | 2 | 4.6 | 6.47 | ok |
| Brake piston | 105 x 105 x 6.34 | 2 | 3.4 | 13.94 | ok |
| Tailstock plate | 110 x 73 x 25.40 | 2 | 18.6 | 6.47 | ok |
| Tailstock base | 102 x 110 x 19.05 | 2 | 10.6 | 8.98 | ok |

Not covered by the check: countersinks for the four M8 that come up through the base (90 degree chamfer mill, as iteration 6), and edge breaks. Steel parts are 1018 cold-rolled flat, O1 ground flat stock (discs) and 4140 pre-hardened or O1 (spider); how each is made is in `MAKING.md`.

## 8. On the machine

`scripts/machine_check6.py`: the assembly's STEP in the DMC2 machine model, turned half a turn about Z, faceplate at machine X 39.4, axis at Y -325.3, 63.94 mm above the table; tool setter (Ø40, top 63.8 mm above the table) with its centre on the back-left table corner, and on the front-left corner as a second case; tools of 21, 24 and 40 mm (probe) below the collet nut. rCAD's interference check at every pose.

| Kind | Poses | Clear | Hits |
|---|---|---|---|
| must be clear | 66 | 65 | Y sweep, head parked left, dy -150.7: base 4 x table 16, base 4 x table 17, base 4 x table 20 |
| probe over a 90 mm part | 3 | 3 |  |
| probe over the headstock (not needed) | 8 | 5 | Z top over the vise, 0 mm off the axis, probe40 mm tool: tool probe40 x swing envelope: vise; Z top over the faceplate and nose, 0 mm off the axis, probe40 mm tool: A: Faceplate (steel) x tool probe40, A: Spindle nose (steel) x tool probe40, A: Splash ring x tool probe40; Z top over the plates, 0 mm off the axis, probe40 mm tool: A: Brake piston x tool probe40, A: C1 piston and seal plate x tool probe40 |
| front-left case | 6 | 6 |  |

The 'Y sweep at dy -150.7' hit is the machine model's own base against its own table at the end of Y travel; it is there without the A axis too.

Rules that follow from the machine's shape (they apply to every variant):

- A retracted 24 mm tool's tip is 126 mm above the table and the unit is 120 mm tall, so the head can pass over the whole unit with Z up. The 40 mm probe's tip is 110 mm up: it passes beside the headstock, not over it.
- Small work close to the jaws is reachable: the top of a 20 mm part 14 mm past the jaw tips, and its side at centre height 50 mm out, are clear with the 24 mm tool at A 0 and A 90.

## 9. Making it

See `MAKING.md`: the accuracy each feature needs, the five machine checks that come first (reversal error, scale, squareness, probe calibration, run-out), the probe-and-correct loop, each part, assembly and preload setting, in-situ calibration and its limits, and the list of what rcnc needs.

## 10. Upgrade path to the bought strain-wave unit

If the cycloid's position error turns out not to be good enough, the front end stays: base, P1 with the RU66, faceplate, nose, brake, C1, hub disc A, brake disc, hub spacer, tie-rod pattern, tailstock. Taken out: carrier 1, G1, M, G2, the discs, shaft, spider, plate D with the motor. Put in: an adapter disc in carrier 1's place (six M6 nuts on the hub side, eight M8 clearance holes on a 42 mm circle into the HSS-25-100's output), iteration 6's spacer plate C2 and motor plate D around the reducer, and either the NEMA 34 in line or the same NEMA 23 on the belt (next section). The tie rods are the same four M8; the unit grows 36 mm in the first case. The adapter disc is not modelled; the rest is iteration 6 as built in `../iter6/`.

## 11. The three drives side by side

| | Strain wave, NEMA 34 in line (iteration 6) | Strain wave, NEMA 23 on a belt (numbers only) | Cycloid, NEMA 23 on a belt (c4) |
|---|---|---|---|
| Ratio | 100:1 | 100:1 with a 1:1 belt (150:1 with 20:30 teeth) | 69:1 |
| Length, jaw tips to rear end | 290 mm | about 190 mm | 211 mm |
| Backlash | at most 20 arc-sec (D) | the same; the belt's 0.02 mm is 0.04 arc-sec at the table | zero below the 6 N·m preload; above it the brake is on |
| Lost motion on reversal under load | 60 arc-sec hysteresis (D) | the same | friction of the preloaded mesh; not calculated, to be measured |
| Position error | 60 arc-sec (D) | the same | 226 arc-sec peak to peak for probed parts (M); about 60 after mapping (G) |
| Torsional stiffness | 3.1e4 to 5.7e4 N·m/rad (D) | the same (belt reflected: 5e6) | 2.6e+04 N·m/rad (C) |
| Windup, finishing at 46 mm, no brake | 5.3 µm | the same | 6.4 µm |
| Motor torque margin at the roughing peak | 6.0 at 600 rpm (mean roughing) | 2.2 at 300 rpm, 1.4 at 900 rpm | 1.4 at 300 rpm, 0.9 at 900 rpm |
| Table speed at 900 motor rpm | 54 deg/s | 54 deg/s (1:1) | 78 deg/s |
| Parts to make | 13 | 13 plus a pulley hub and a guard | 21 |
| Bought drive parts | reducer 269 USD, NEMA 34 IP65 126, drive 46 | reducer 269, NEMA 23 IP65 and drive about 110 (est), belt and pulleys 35, bearing 10 | NEMA 23 IP65 and drive about 110 (est), bearings, pins, sleeves, shaft, belt and pulleys about 130 (est) |
| What can go wrong | pre-order lead time; the vendor's accuracy claim | as left, plus one more belt | everything in the tolerance table; it stands or falls with the making |

**Belt to the strain-wave unit** (asked for as a variant, then set aside): sound on paper. A hub with a Ø20 journal in a 6004 bearing bolts to the wave generator's four M5; the NEMA 23 sits beside the housing as in the cycloid. The motor needs 0.54 N·m at the roughing peak with a 1:1 belt and has 1.21 at 300 rpm (D), so no reduction is needed in the belt; everything the belt does wrong is divided by 100. It saves about 100 mm of length and 0.9 kg hanging off the back. It was not modelled further because Jonas moved the effort to the cycloid.

**My recommendation**: build the cycloid as the project it is meant to be, with the machine checks of MAKING.md done first; keep the brake and the front end exactly as drawn so that the strain-wave unit can go in later if the measured position error disappoints.

## 12. Review: what I am not sure of

- Position error after mapping (about 1 arc-min) is an estimate; the study itself gives 3.8 arc-min before mapping for probed parts.
- Whether 0.008 mm is reachable depends on the DMC2's reversal error, squareness and the probe's repeatability, none of which I know (question 1).
- Efficiency 0.70 (G). If it is 0.5 the motor margin at the roughing peak falls below 1 without the brake; with the brake on, as intended, it does not matter.
- Radial stiffness of the 6904 bearings (1e8 N/m, G) sets the drive's stiffness; the figure could be 30 % either way.
- Blade preload: stiffness goes with the cube of the 2 mm blade thickness. Measure the blades; set the preload by torque, not by angle.
- The lip seal runs on a milled and polished steel rim at under 15 rpm; fine for grease, not for oil.
- Friction (stick-slip) of the preloaded mesh at very low speed is not modelled; it shows as a small reversal error that the brake does not remove.
- Soft steel discs on hardened pins will wear in. That helps evenness and costs preload; the preload is reset after run-in, and its swing is the wear indicator.
- The front 6001 sits in a blind pocket in carrier 1; to get it out again the pocket needs two small notches for a puller, not drawn.
- RU66 drawn from THK's dimension table (no vendor CAD); a clone bearing's accuracy is the vendor's claim.
- Shop air, brake friction coefficient and the lining are as iteration 5 and 6: assumptions until tried.
- The motor on its mount is the lowest mode (about 200 Hz with the real motor). The belt's tooth frequency crosses it at about 600 motor rpm; if it sings there, add the wedge between motor and housing.
- Prices are estimates.

## 13. Files

| File | What |
|---|---|
| `REPORT.md` | this report |
| `MAKING.md` | making plan per part, probe-and-correct, calibration, rcnc requirement list |
| `bom.csv` | bill of materials for c4 |
| `cycloid.md, cycloid.json, cycloid-profile.png` | tolerance study and the disc outline |
| `cycloid-pass2-independent-discs.md` | the same study with the discs cut one at a time and on separate pins (pessimistic) |
| `c1/ to c4/` | each iteration: rCAD document a-axis.json, parts as STEP and PNG, renders, calc.md, dfm.json, FEA, machine check |
| `renders/, step/` | pictures and STEP of the final iteration |
| `../scripts/buildc.py, spider.py, cycloid.py, calcc.py, feac.py, spider_fea.py, extrasc.py, bomc.py, reportc.py` | everything is regenerated by these |
| `../iter6/` | the low headstock with the bought strain-wave unit (comparison and upgrade path) |
