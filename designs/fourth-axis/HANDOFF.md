# DMC2 4th (A) axis: handoff

Written 2026-11-23 from the thread "Design a zero-backlash 4th axis". Status: **design only**. Nothing is bought, cut or run on the mill, and no CAM is written.

## What Jonas asked for

- A rotary 4th axis small enough to fit on the DMC2 bed, with every custom part made on this mill.
- No backlash, several fully simulated iterations, "not done till it's perfect".
- Workholding on the axis is a **XinDian H54-8-55 self-centering vise** (not a chuck).
- **The tool setter must still work** with the axis installed.
- Steel machined parts are allowed if they make it smaller.
- Jonas chose to **focus on a home-machined cycloidal gearbox** ("a good project", "we can hold tighter tolerances if we are careful"), using **in-situ probing and correcting**.
- Since then Jonas has asked twice about cheaper harmonic gearboxes, so the cycloidal versus harmonic choice is not fully closed.

## Where everything is

| What | Where |
|---|---|
| Project files (this folder) | `fourth-axis/`: `REPORT.md`, `bom.csv`, `renders/`, `step/` for the harmonic design (iterations 1 to 6); `fourth-axis/cycloidal/`: `REPORT.md`, `MAKING.md`, `bom.csv`, `renders/`, `step/` |
| Full working tree, every iteration, scripts, FEA | netdash, `~/dmc2/fourth-axis/` (cycloidal in `cycloidal/c1` to `c4`) |
| Git | `JonasMerrell/cad-rust-app`, branch `claude/project-thread-33amtt`, folder `designs/fourth-axis/`. Head `1fcf1c2`. Not merged, no PR. |
| rCAD change | Commit `a4045f6` on that branch adds a `probes` option to `solve_simulation` (FEA displacement at chosen points). Unmerged. |
| Project memory | `dmc2-fourth-axis` |

Read `cycloidal/REPORT.md` first, then `cycloidal/MAKING.md`, then sections 14 and 15 of the top-level `REPORT.md` (iteration 6 and corrections).

## Facts measured from the machine model

- Table 350 x 250 mm. The spindle reaches only the back 168 mm in Y. Collet nut to table is 150 mm at Z top, so a 24 mm stick-out tool tip retracts to 126 mm, and the 40 mm probe to 110 mm.
- The two-station vise leaves no room, so the A axis **replaces the vise** on the M8 fixture plate. No new holes.
- The fixture grid hole positions are not in the CAD, so placement is good to +/- 12.5 mm until one hole is measured.
- Tool setter: rcnc has it at machine X -5, Y 289, top 63.8 mm above the table. Jonas says its centre is on the table corner. Back-left is assumed from rcnc's numbers (the CAD shows front-left, outside Y reach). Jonas has not confirmed which corner.
- XinDian vise: 75 x 54 x 44 mm, about 1.1 kg, 4x M6 on a 40 mm circle, swings 92 mm. Modelled from vendor dimension drawings, since no STEP exists.
- Loads used: roughing 850 W at 9000 rpm gives 391 N mean sideways (about 780 N peak), 18 N·m mean and 36 N·m peak at the 46 mm radius. Finishing at 24k gives 78 N and 3.6 N·m.

## Current main design: cycloidal c4

- Front end shared with harmonic iteration 6: 1018 steel faceplate carrying the vise, RU66 crossed-roller main bearing (clone), pneumatic disc brake, home sensor, tailstock (12 mm spot drill as dead centre). 120 mm tall, headstock at the **right** end of the table, nothing overhangs the table.
- Reducer: 23:1 cycloid, 24 ring pins (8 mm dowels on an 88 mm circle), eccentricity 1.25 mm, two 6.35 mm O1 discs cut stacked. A flexure "spider" carrier springs one disc against the other at about 6 N·m.
- Drive: NEMA 23 closed-loop IP65 (23E1KIP65-20) beside the housing, 3:1 GT belt, 69:1 overall. 211 mm long.
- 210 parts and screws, no overlaps. 21 machined parts, all pass the one-cutter (1/4 in endmill) check. No tapped holes.

| Check | Result |
|---|---|
| Backlash | zero below the 6 N·m preload; finishing needs 3.6 N·m |
| Roughing, brake on | 3.4 µm windup at peak |
| Deflection at the jaws | 14.4 µm roughing, 2.9 µm finishing (targets 20 and 5) |
| 100 mm overhang, no tailstock | 61 µm, so long parts need the tailstock |
| Position error, parts at 0.008 mm | 3.8 arc-min peak to peak (about ±25 µm at 46 mm radius) |
| Drive stiffness | 2.6e4 N·m/rad |
| Motor margin | 2.7 mean roughing, 1.4 roughing peak, 9.3 finishing |
| Lowest mode | motor on its mount, about 200 to 255 Hz; tool-point modes 670 and 790 Hz |
| Tool setter | touch-offs and routes clear for 21 and 24 mm tools and the 40 mm probe; the probe goes beside the headstock, not over it |
| BOM | about 1,190 USD (530 stock, 660 bought), mostly estimates |

Tolerance study (Monte Carlo): lost motion without preload is 36 arc-min as milled (0.03 mm), 21 at 0.015, 8.5 at 0.008 (probed and corrected), 4.6 at 0.004. Position error is 14, 8, 3.8 and 2.1 arc-min. Preload removes play, not position error. A correction table in rcnc might bring it to about 1 arc-min, which is a guess until a real unit is measured.

## Alternative: harmonic (strain-wave) design

- StepperOnline HSS-25-100-I-D14, size 25, 100:1, 269 USD: backlash 20 arc-sec, about 1 arc-min position error, stiffness 3.1e4 to 5.7e4 N·m/rad. 13 machined parts.
- Iteration 5 (aluminium, NEMA 34 in line, 142 mm tall) is fully documented but blocked the tool setter. Iteration 6 fixed that (120 mm, turned end for end, RU66, steel faceplate) and has corrected FEA and machine checks, but no modal run or BOM of its own.
- A NEMA 23 on a 1:1 belt driving the size 25 unit is sound on paper (about 190 mm long) but exists as numbers only. It is not modelled.
- The cycloidal front end accepts the harmonic unit later with one adapter disc, which is not modelled.
- Cheaper harmonic options: new units are all within about 50 USD of each other, so only a used genuine unit (CSF-25-100-2UH or CSF-20-100-2UH) is clearly cheaper. Used prices are unconfirmed.

## Open questions for Jonas

1. What measuring tools does Jonas have (micrometers and ranges, gauge pins, gauge or 1-2-3 blocks, dial test indicator, ring gauge), and have the probe's repeatability and the mill's X/Y reversal error been measured? This decides which tolerance row is realistic.
2. Is shop air at about 6 bar acceptable for the brake? The fallback is a manual clamp screw.
3. Cycloidal or harmonic as the build? Jonas sent an AliExpress listing (item 3256812979878137, about 138 USD, or 80 with a new-user discount) that could not be opened. Jonas was asked to paste its title and specs. Needed: strain-wave type, frame size (20 or 25 preferred, 17 workable), ratio, complete unit with crossed-roller output or bare set, torque, backlash, input bore.
4. Which table corner is the tool setter on (back-left assumed), and where is one fixture-grid hole?

## Next steps

1. Get answers to the questions above.
2. Before cutting any cycloid part, run the five machine checks in `cycloidal/MAKING.md`: reversal error, scale, squareness, probe calibration, run-out.
3. Write CAM for the parts. Read `guidelines/guidelines.md` first, and rCAD must not post a program whose sim shows gouges.
4. rcnc work (requirement list at the end of `MAKING.md`, nothing implemented): A axis on step/dir, brake M-codes with interlock, homing on the inductive sensor, probing cycles with logging, a tool radius correction the finish pass reads, probe-compute-recut loops, a rotary correction table, and scale, squareness and backlash compensation.
5. Optional: draw the wedge that stiffens the motor mount, model the harmonic adapter disc, and model the NEMA 23 belt-to-harmonic variant in full if Jonas goes that way.

## Known weak points and caveats

- Steel on this mill is unproven. The spindle has roughly 400 W at steel speeds, so steel parts are small, flat and lightly cut.
- The brake's friction, air pressure, thin disc web and piston return are assumptions. The report gives a torque-wrench test for assembly.
- Bearing and bolted-joint stiffness are calculated or guessed, not vendor data.
- The RU66 is drawn from THK's dimension table (no vendor STEP). The vise, home sensor, spot drill and O-rings are also not vendor CAD.
- Most cycloidal BOM prices are estimates, because StepperOnline's pages refused automated reads.
- Corrections already made: an early "5 arc-min as milled" estimate for the cycloid was wrong (36). Structure FEA had left a part out of the mesh (iteration 5 is 9.5 µm at the jaws, not 13.6). The machine check had the turned unit facing the wrong way in half the poses (fixed and rerun for iteration 6, c3 and c4). Modal runs for iterations 1 to 5 were not repeated after the FEA fix.

## How the work was run

All modelling and simulation ran on netdash through a Remote Control session in `/home/jonas`. rCAD's FEA needs the iterative solver (about 8 GB and 2 minutes per load case); the direct solver used about 30 GB per run and the modal run needs about 30 GB on its own.
