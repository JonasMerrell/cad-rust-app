# DeWalt DC742 drill: parametric rCAD model, v2 (detail pass)

This is a native rCAD Part Studio with 65 named, coloured parts. It's built only from rCAD features: revolves, lofts through measured section sketches, extrudes, sketch text, circular patterns, booleans and transforms. The drill scan is in the document as a hidden reference body ("Scan reference"); unhide it to compare.

Frame: the scan_photos result frame, in mm, at 80.9 mm/unit (from the block-edge clicks). The drill stands on XY, Z is up and the chuck points to -X.
`gen2.py` writes the `rcad run` script. Every dimension is in it, and `sections.json` holds the measured section outlines.

## Files

| File | What |
|---|---|
| `dc742-drill.json` | rCAD document: full feature history plus the hidden scan reference |
| `dc742-drill.step` | STEP of the 65 parts (no scan) |
| `dc742-drill.stl` | STL of the model (no scan), as used for the fit check |
| `dc742-drill.html` | rCAD `export_web` interactive viewer |
| `dc742-drill-*.png` | renders: iso, iso_front_left, iso_back_right, left (-Y), front, back, top |
| `closeup-*.png` | close-ups: chuck, clutch, speed selector, logo side, label side, grip + trigger, battery, rear vents |
| `gen2.py`, `blockfont.py`, `sections.json` | generator, block-stroke lettering, measured sections |

## Fit to the scan

Distance from each scan point (the fused COLMAP cloud in the same frame) to the model surface. Lower is better; v1 is shown for comparison.

| Region | Points | Median | p90 | p99 | v1 median / p90 / p99 |
|---|---|---|---|---|---|
| Chuck + clutch | 11,869 | 1.51 | 4.80 | 10.64 | 1.61 / 4.81 / 10.71 |
| Housing | 23,364 | 1.49 | 5.61 | 14.18 | 1.51 / 5.67 / 14.36 |
| Rear cap | 9,246 | 2.05 | 5.93 | 10.11 | 2.27 / 6.09 / 10.42 |
| Grip + trigger | 43,535 | 1.25 | 3.91 | 7.67 | 2.10 / 5.26 / 9.21 |
| Battery | 118,059 | 1.59 | 4.26 | 9.49 | 2.09 / 6.02 / 10.47 |
| **Whole drill** | 206,668 | **1.53** | **4.42** | **10.47** | 2.01 / 5.83 / 10.83 |

All values are in mm, and every region is as good as v1 or better.

The housing p99 (14 mm) comes from points that sit about 10 mm *inside* the shell, under the glossy black DC742 rating label. The scan's depth estimate fails on that shiny surface, and no geometry change can fix it.
The large deviations in v1 were mostly points inside the model: the grip and battery were too fat. They are now lofts through sections measured from the scan, with the battery sections fitted to the points by least median distance and smoothed along Z.

The scan's own noise (point to scan mesh) is about 0.9 mm median. The black matte surfaces (overmould, battery base) scatter points about 2 to 5 mm inward, and that inward scatter makes up most of the remaining p99.

## Details

"Measured" means fitted to the scan points. "Photo" means sized from the six reference photos against the measured geometry next to it.

### Chuck and clutch
| Detail | Source |
|---|---|
| Head axis: position (Y 22.5, Z 203.9 at x 0) and the 5.7° tilt | measured |
| Chuck nose profile (stepped cone) | measured |
| Jaw opening (Ø9.6) and the three jaw tips | photo |
| Front sleeve (Ø39.6) and its 16 longitudinal grip ribs | sleeve measured, ribs photo |
| Rear sleeve (Ø41) and its 24 grip ribs | sleeve measured, ribs photo |
| Silver ring between the chuck and the clutch | photo (position measured) |
| Clutch collar cone with raised number band | cone measured, band photo |
| Clutch numbers 3 5 7 9 (+Y side) and 1 17 15 13 (-Y side), raised block-stroke lettering | photo |

### Housing
| Detail | Source |
|---|---|
| Motor/gear housing revolve (Ø55 to 63.6) | measured |
| Grip-to-housing junction: the measured z 166 section carried up into the housing (a section above z 166 made the loft overshoot, and the z 174/182 outlines pick up the housing bottom) | measured |
| Raised screw bosses on the upper shoulders beside the speed selector (4, yellow) with black caps | position from the scan (points about 8 mm proud at 45°), size from the photos |
| Parting-line grooves top and bottom, where the shell halves meet | photo |
| Speed selector: raised frame, pocket, black slider with a grip ridge, "1" and "2" markings | photo |
| Rear screw caps (2) | photo |
| Screw counterbores and holes on the -Y half (4: two rear bosses, front-bottom, rear-bottom) | photo (visible ones only; the +Y half has no screw heads) |
| DEWALT logo panel: flat recess, black plate, raised DEWALT lettering (-Y) | photo |
| DC742 rating label panel: flat recess, black plate, raised DC742 lettering (+Y) | photo |
| Rear cap (black) | measured |
| Vent grille inserts (yellow ovals on both sides) with 5 slots each | photo |
| Rear-top vent slots (5) | photo |

### Grip, trigger, button
| Detail | Source |
|---|---|
| Grip core: loft through 9 measured sections (Fourier-smoothed), with its top carried into the housing | measured |
| Rubber overmould as its own black part, outline from the photos (front yellow strip, yellow base ring) | sections measured, outline photo |
| Trigger profile | measured |
| Trigger texture ribs (5) | photo |
| Forward/reverse button, capsule-shaped, proud on both sides | photo |

### Battery (DC9071-style 12 V pack)
| Detail | Source |
|---|---|
| Black base with tapered foot: lofted rounded-rectangle sections fitted to the points, then smoothed along Z | measured |
| Yellow top and the stem into the grip, lofted sections | measured |
| Label recess with raised "12" and "VOLT" print (-Y) | photo |
| Release buttons on both sides | photo |
| Black latch ridges under each button (3 each side) | photo |

### Not modelled, and why
- **Belt hook mounts and LED:** the DC742 has neither.
- **Battery terminal slot:** it sits inside the grip when the pack is fitted, so no photo shows it.
- **Warning-label text and the small rating-label print ("3/8" (10mm) CORDLESS DRILL DRIVER"):** too small to read reliably in the photos, and too costly to rebuild.
- **Battery foot grooves:** the photos don't clearly show grooves on the black base, and a test version of the cut misbehaved, so I left them out.
- **Screw positions on the -Y half that the overmould or angle hides:** only the visible ones are modelled.
- **Battery bottom features:** the bottom is never visible (it stood on the carpet).
- **Edge fillets:** they come from the lofted and revolved shapes (rounded sections and profiles). Separate fillet features weren't added on the text-heavy bodies, because each one slows the whole-history rebuild further.

### rCAD issues found while building this
- **Sketch text drops strokes:** wherever a font's glyph outlines overlap (the stems of D, E, T…), those parts go missing. I worked around it with `blockfont.py`, which draws block-stroke glyphs as closed polylines.
- **Every feature rebuilds the whole history:** this model takes about 1 h 45 min to build from scratch (356 steps). Use `set_parts` (one rebuild) rather than one `set_part` per part.
- **Loft overshoot:** a loft through spline sections overshoots badly when the last section repeats an earlier outline or sits far above the others. End the loft at the last measured section and extrude it instead.
- **Extrude `op: add` fuses touching bodies:** for example the housing and the rear cap. Use `new`, then a `boolean` union with a named target.
