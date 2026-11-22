# DeWalt DC742 drill: parametric rCAD model

This was built with rCAD (photogrammetry-colmap4 build) entirely from native features: revolves, lofts, extrudes, boolean cuts, circular patterns and transforms. There are 28 named, coloured parts. The drill scan is also in the document, as a hidden reference body.

Frame: the scan_photos result frame, in mm, at a scale of 80.9 mm/unit (from the block-edge clicks). The drill stands on XY with Z up, and the chuck points to -X.

## Files

| File | What |
|---|---|
| `dc742-drill.json` | rCAD document: the full feature history, plus the scan reference (hidden; unhide "Scan reference" to compare) |
| `dc742-drill.step` | STEP, the 28 solid parts (no scan) |
| `dc742-drill.stl` | STL of the model (no scan), as used for the fit check |
| `dc742-drill.html` | rCAD `export_web` interactive viewer |
| `dc742-drill-*.png` | renders: iso, iso_front_left, left (-Y), front, top, back |
| `gen.py` | generator for the `rcad run` script: every dimension is here, so edit and rebuild |

## Fit to the scan

For each scan point (the fused COLMAP cloud in the same frame), I took the distance to the model surface. For reference, the scan's own noise (point to scan mesh) is about 0.9 mm median.

| Region | Points | Median | p90 |
|---|---|---|---|
| Chuck + clutch (x < 10) | 11,869 | 1.61 mm | 4.81 mm |
| Housing (x 10 to 112) | 23,364 | 1.51 mm | 5.67 mm |
| Rear cap (x > 112) | 9,246 | 2.27 mm | 6.09 mm |
| Grip + trigger (z 78 to 176) | 43,535 | 2.10 mm | 5.26 mm |
| Battery (inside its footprint) | 118,059 | 2.09 mm | 6.02 mm |
| **Whole drill** | 206,668 | **2.01 mm** | **5.83 mm** |

The housing's p99 is 14 mm. The real housing has a lower "belly" where it runs into the grip, behind the trigger, and my circular revolve doesn't model it.

## Features

| Feature | Source | How |
|---|---|---|
| Head axis: position (Y 22.5, Z 203.9 at x 0) and 5.7° tilt | measured (points) | circle fits along the head |
| Chuck nose / jaws | measured | revolve |
| Chuck front sleeve (Ø39.6) + 16 grip ribs | sleeve measured; ribs estimated (photos) | revolve, groove tool, circular pattern, boolean subtract |
| Chuck rear sleeve (Ø41) + 24 grip ribs | sleeve measured; ribs estimated | as above |
| Silver ring | estimated (photo), position from points | revolve |
| Clutch collar with raised number band | cone measured; band estimated | revolve |
| Motor housing (Ø60 to 63.6) | measured | revolve |
| Rear cap | measured | revolve |
| Speed selector: raised frame, pocket, black slider | estimated (photo; scaled from the housing Ø) | extrude, union, pocket, extrude |
| Screw caps (4) | estimated | extrudes |
| Side vents on the rear cap, 5 slots each side | estimated | cut extrudes |
| Rear-top vent slots (5) | estimated | cut extrude |
| DEWALT logo panel recess + black plate (-Y) | estimated | cut + plate |
| DC742 rating label recess + black plate (+Y) | estimated | cut + plate |
| Grip (rubber overmould) | measured: 5 elliptical sections | loft |
| Yellow front spine of the grip | outline estimated, front edge from points | extrude |
| Trigger | measured (front profile from points) | extrude |
| Forward/reverse button | estimated (photo) | extrude |
| Battery base with tapered foot | measured (footprint and taper) | loft + extrude + union |
| Battery collar (yellow) | measured | extrude |
| Battery stem into the grip | measured (z 62 to 78 sections) | loft |
| Battery release buttons (±Y) | estimated | extrudes |
| Battery latch ridges (3 each side) | estimated | extrudes |
| Battery label recess (-Y) | estimated | cut |

Not modelled:
- **Belt hook mounts and LED:** the DC742 has neither.
- **Clutch numbers and DEWALT lettering:** only the raised band and the plates are modelled, not the text.
- **Rear oval vent grille's yellow insert:** only the slots are cut.
- **Housing belly under the motor:** see the fit note above.
- **Exact two-tone outline of the grip overmould:** approximated by a black grip with a yellow front spine.
