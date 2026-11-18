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

## Feed scheduling (`feed_schedule`, src/cam_feed_schedule.rs)

The same model, run backwards: keep the toolpath, and set each cutting move's feed so the spindle works at a
target power (feedrate scheduling, Erdim, Lazoglu & Ozturk 2006; Altintas, *Manufacturing Automation* ch. 2).
Off unless an op (`cam_op feed_schedule`) or the setup (`create_cam` / `edit_cam` `feed_schedule`) turns it on;
only roughing ops (adaptive, pocket, clear, face, contour with `leave` > 0) are touched.

1. After the toolpaths and stay-down links are made, feed moves longer than 2 mm are split into pieces (plunges
   and ramps steeper than 1 in 2 are left alone: the end of the tool cuts there).
2. The whole setup is simulated once with a load profile: every load sample (width, depth, cross-section over the
   last millimetre) and the move it belongs to. The geometry doesn't depend on the feed, so one run gives every
   piece's cut.
3. For each sample, the highest feed at which the cut takes no more than the target and stays within every other
   limit (force, bend, stress, torque against the never-exceed share, chip; worst use ≤ 0.97), found by bisection
   on `cam_cutting::cut` (P ∝ vf^(1−mc), vf^0.75 in aluminium). The width is taken half a grid cell wider: on
   another grid (the export's own simulation) the stock's edge falls a cell either way, ±10 % of a 2 mm cut.
4. A piece takes the lowest feed any sample of it, or of the path within one tool radius ahead, needs (the tool
   slows before it reaches a wall). Pieces cutting nothing run at the ceiling.
5. Bounds: ceiling `feed_max_scale` (default 2) × the programmed feed and the machine's `max_feed`; floor
   0.3 × the programmed feed and a 0.02 mm chip per tooth.
6. Feeds are rounded down (10 mm/min steps under 400, 25 under 1000, 50 above: ≤ 5 %), runs shorter than 3 mm
   faster than a neighbour come down to it, and the pieces of one move at one feed are joined again so arc fitting
   finds the same runs.
7. Re-simulated; a piece still more than 2 % over the target is lowered by (target / P)^(1/(1−mc)) (up to 3 rounds).

Target and cap are the power budget's (`cam_paths::power_budget`): the op's `power_target` / `power_cap`, else
the setup's `feed_target`, else 60 % of the power the spindle has at the tool's speed, the cap its caution share
(on the DMC2 at F004 = 150: 671 / 850 W at 9000). `power_target` without `feed_schedule` uses the same code in
reduce-only mode: feeds only come down to the cap, never under a quarter of the programmed feed.

Reported per op (the op's warnings, `simulate_cam` `feed_schedule`, the setup sheet): time, cutting time, peak
and time-weighted mean power while cutting before and after, and the share of the cut at the feed ceiling. At
9000 rpm with 3 flutes the 3000 mm/min ceiling is 0.11 mm/tooth: where much of the cut sits there, a wider
stepover (not a faster feed) is what would take more.

### Link feed (`machine.link_feed`)

With `link_feed` over `max_feed`, after the feed schedule (and whether or not it ran), the setup is simulated once
more with the material each move removes: a G1 move that removes under 0.05 mm² per mm and takes no load sample
cuts nothing (a link through cleared ground, a stay-down link, a move in air) and runs at `link_feed`. Long moves
are split first so their first 1.5 mm (the load check looks a millimetre back) and last 1 mm keep the cutting
feed; a move right before a cut, a plunge or a ramp keeps its feed, and so do plunges, ramps, probing, drilling
and tapping. A second simulation puts any raised move that takes a load after all back to `max_feed`. Cutting
moves stay at `max_feed` or under. G0 rapids run at the controller's own speed; `rapid_rate` (X/Y) and
`machine.rapid_z` only set the time estimates.

## Calibration (DMC2, 6061-T6, 1/4" 3-flute carbide at 24 000 rpm, 24 out of the holder)

**Measured, 2026-11-17** (the stick-out gauge's broken-tool run): the spindle's speed was heard from a
transducer on the spindle head. The cuts were 6 deep at F2400 from the edge of a 32 mm bar.

| Cut | MRR cm³/min | Model W | Heard | Meaning |
|---|---|---|---|---|
| 0.6 × 6 | 8.6 | 351 | 1.8% under the air speed, steady | fine |
| 1.0 × 6 | 14.4 | 531 | 3.2%, steady | fine: the planning target |
| 1.4 × 6 | 20.2 | 647 | 7.5% and never settled | **at pull-out** |
| 1.8 × 6 | 25.9 | 872 | ran away below 300 Hz in 1.4 s | broke the tool |
| 0.6 × 10 at 2500 (Jonas) | 15.0 | about 550 | comfortable | |
| 0.6 × 12 at 2500 (Jonas) | 18.0 | 656 | on the edge | |

So for the DMC2:
- the spindle's power is its **pull-out at about 645 W of this model's watts at 24k**: `spindle_power` 1500 ×
  `spindle_efficiency` 0.43;
- **`spindle_never_exceed` 0.85 (550 W)**: a cut past it is a problem;
- **`spindle_caution` 0.775 (500 W)**;
- the adaptive's 0.7 power share (450 W) is the bulk of a cut;
- below 24k the torque is taken as constant, which is **not measured yet**; the rpm sweep in the next
  calibration run measures it.

The earlier observations disagree with this, so the model's scaling between cut shapes isn't right yet:
- the test part's 4.65 × 3 at 2400 (995 W by the model) and the phone stand's 1 × 12.5 finish (1068 W) both
  ran;
- wide and shallow, or short engagements carried by the spindle's inertia, may cost less than the model says.

Until that's measured, keep the 1/4" at a stepover of about 1 mm or less and **14–15 cm³/min (never over 18)**.
Run every cut beyond a proven one with rcnc's bog guard armed (`(RCNC REQUIRE_GUARD)`).

`cam_cutting::tests::calibrated_on_the_dmc2` holds these points.

## Chip evacuation (a heuristic risk index, not chip physics)

Code: `cam_cutting::ChipMap`, `Sim::chip_sample`, `OpChips`.

- Confinement at each tool position: in 16 directions, whether stock stands more than 1 mm above the
  tool's tip within two diameters of its edge (re-measured every few positions); `conf` = share of
  directions walled in, `depth` = their mean height. Cutting time is split open (conf < 0.4), slot
  (0.4–0.75) and pocket (≥ 0.75).
- Chips made: volume removed × `BULK` (12). Share that stays near the tool: conf² × (1 − 0.6·throw), with
  throw = mean chip thickness / `THROW_CHIP` (0.02 mm), between 0.15 and 1: thin, light chips (light
  high-speed stepovers) aren't thrown clear, thick ones are (Jonas: "worse with thin light chips").
- They are spread over the 2 mm cells within one diameter of the tool. Each cell drains exponentially at
  a rate set by the machine's chip clearing (`Machine::chip_clearing`: low-pressure flood 0.4/s, flood 1.0,
  air 0.6, none or coolant off 0.05), times exp(−depth / 1.5 D): deep cavities hardly drain (Jonas: "deep
  pockets were bad", low-pressure flood).
- Index = chips in a cell ÷ (cell area × cavity depth) × depth weight (depth / 1.5 D, 0.1–2: chips in a
  shallow cavity spill over rather than pack) × `RISK_SCALE`. `RISK_SCALE` (0.635) puts the phone stand's
  op 1, which ran with no chip trouble, at 0.4. Above 0.5: caution; above 1: chips pack in.
- Also counted: distance cut through cells over 0.5 (re-cutting packed chips), and full-width slots deeper
  than the diameter.
- To tune: BULK, THROW_CHIP, the clearing rates and RISK_SCALE, from cuts where chips did or didn't pack
  (add them as tests like `calibrated_on_the_dmc2`).

## Chatter (stability lobes): trend-accurate only, until the tool is tap-tested

Code: `cam_cutting::tool_mode`, `stable_depth`, `CHATTER_SCALE`; `Sim` `OpChatter`.

- The tool's first bending mode at its tip: the stepped cantilever above (stiffness 1/compliance),
  mass ρ·volume (carbide 14.5 g/cm³, steel 8.0), 0.243 of it at the tip (uniform cantilever) →
  f = √(k/m)/2π; in series with the holder and spindle (`Machine::holder_stiffness`, 20 N/µm assumed),
  damping ratio `Machine::tool_damping` (0.03). A tool's measured `mode_hz`, `mode_k`, `mode_zeta`
  (tap test) replace the computed ones. The 1/4" carbide 24 out: about 7.5 kHz, 4.6 N/µm.
- Zero-order milling stability (Altintas & Budak, 1995): the average directional coefficients for the
  cut's entry and exit angles (climb or conventional), the tool equally flexible in X and Y, Kt = kc at the
  mean chip, Kr = the material's radial ratio. For chatter frequencies from 1 to 2.5 × f the limiting
  depth a_lim = −2π·Re(Λ)/(N·Kt)·(1 + κ²) and the spindle speed n = 60·ωc/(N·(π − 2·atan κ + 2πk)) per
  lobe k; the stable depth at the programmed speed is the lowest lobe crossing it.
- The honest part: with the assumed holder stiffness and damping these lobes put every clean cut the DMC2
  has made 8–12× past the limit, so the real tool-point response is much better than assumed.
  `CHATTER_SCALE` (0.078) scales the margin (depth ÷ stable depth) so the phone stand's op 1 cuts that ran
  clean read about 0.8–0.9: it is an index relative to proven cuts. Above 0.7: caution; above 1: expected
  to chatter. The suggested nearby spindle speed and the effect of 5 mm less stick-out are trends.
- To make it absolute: a tap test per tool and stick-out (instrumented hammer and an accelerometer, or a
  microphone: tap the tool, record the ring, the peak frequency and its decay give f and ζ; the stiffness
  needs a known force or a static deflection measurement), and observed chatter / no-chatter cuts as tests.

## Listening on the machine (rcnc: built, waiting for the microphone)

rcnc's `cnc-listen` crate and the app's Settings → Listening write `<program>.cutlog.json` beside each
program run; rCAD's `import_cutlog` reads it back for a setup: per operation the heard speed droop and
chatter against the model's spindle power and chatter margin, and suggests (or with `apply` sets) the
machine's `spindle_efficiency` (droop ÷ `spindle_rated_slip` = the share of shaft power used, so
efficiency = model power ÷ (that share × input power available at the speed), the median over operations)
and `chatter_scale` (the quietest chattering cut reads 1; flagged if a clean cut would then read 1 or more).
The design notes it was built from:
- Crates: `cpal` (audio input on Windows WASAPI, Linux ALSA/Pulse) and `rustfft`; `hound` to read WAVs
  in the simulator and tests. An optional feature of the app crate, so a machine without audio builds and
  runs as now.
- Record one or two channels (air microphone and/or a piezo contact pickup on the spindle housing) while
  a program runs; time-stamp each block against the executing line, operation, tool, programmed rpm and
  feed.
- Rolling FFT (e.g. 8192 points at 48 kHz, 50% overlap): find the tooth-passing frequency (rpm·teeth/60)
  and its harmonics: the actual spindle speed from them, so bogging shows as rpm droop under load
  (→ calibrates the spindle's power and efficiency); strong peaks that are not harmonics, near the tool's
  mode → chatter (→ calibrates the mode and damping, and `CHATTER_SCALE`); overall loudness in the cut
  band as a load proxy.
- Log per line next to the job (`<program>.cutlog.json`: line, op, tool, rpm, feed, measured rpm, droop %,
  chatter peak Hz and level, loudness) in a form rCAD's sim reads back as calibration tests.
- A small live indicator in rcnc (rpm droop %, chatter yes/no). No feed override or other motion change
  until Jonas asks.
- A guided tap test at a tool change: tap the tool, record the ring, fit frequency and decay → the tool's
  mode for that stick-out, saved to the tool table and written into the program's tool header next time.
- Concerns: the spindle's own whine (a 24k rpm spindle at 400 Hz plus the VFD's switching), coolant and
  air noise, a microphone's placement and level; Windows device selection; keeping audio capture off the
  real-time control loop (its own thread, a ring buffer).

## Not modelled

 tool wear and edge rounding, thermal
effects, the machine's own stiffness and the workholding (a part can move before the tool bends), the
spindle's speed droop under load (planned with the rcnc microphone), runout.

## Better calibration

The spindle's power is the binding limit on the DMC2, so the best measurement is spindle power or current
under load: a clamp meter on one motor phase or the VFD's own output-current display, read during a steady
cut of known width/depth/feed. Next best: spindle speed droop (a tachometer, or the planned microphone's
tooth-passing frequency). Each reading with its cut goes in as a calibration test.
