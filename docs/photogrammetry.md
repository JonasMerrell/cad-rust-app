# Scan from photos (photogrammetry)

**File → Scan from photos…** turns a set of photos of a part into a mesh reference body in the
Part Studio. You can measure it, section it, sketch on its faces, project its edges and model a
clean part over it. The agent tool `scan_photos` does the same from a script.

## What rCAD runs

rCAD doesn't contain a reconstruction engine. It runs these open-source programs as separate
processes. You install them yourself, and their licences stay with them.

| step | program | licence | hardware |
|---|---|---|---|
| features, matching, camera solve | [COLMAP](https://github.com/colmap/colmap/releases) | BSD | CPU, or faster on an NVIDIA GPU |
| camera solve, global (default when available, often 10× faster) | COLMAP 4 `global_mapper`, or [GLOMAP](https://github.com/colmap/glomap) for older COLMAPs | BSD | CPU |
| dense surface, GPU | COLMAP `patch_match_stereo` + `poisson_mesher` | BSD | NVIDIA GPU (CUDA build of COLMAP) |
| dense surface, CPU | [OpenMVS](https://github.com/cdcseacave/openMVS/releases) `DensifyPointCloud` + `ReconstructMesh` | AGPL | CPU |
| rough surface | COLMAP `delaunay_mesher` on the sparse points | BSD | CPU |

**Automatic** picks COLMAP dense when COLMAP was built with CUDA, otherwise OpenMVS when it's
installed, otherwise the rough sparse mesh.

rCAD finds the programs on the PATH, in a `photogrammetry/colmap` or `photogrammetry/openmvs`
folder next to rCAD, in `/usr/local/bin/OpenMVS`, or at the paths you set under **Programs** in
the window. The `RCAD_COLMAP` and `RCAD_OPENMVS` environment variables work too. The paths are
saved in `~/.config/cad-rust-app/photogrammetry.json`.

- **Windows:** download COLMAP's `COLMAP-x.y-windows-cuda.zip` (or `-nocuda`) and OpenMVS's
  Windows release, and unzip them into `photogrammetry\colmap` and `photogrammetry\openmvs`
  next to `rcad.exe`.
- **Linux:** `apt install colmap` gives a CPU build. Build COLMAP from source for CUDA. Build
  OpenMVS from source using its v2.3.0 tag. On Ubuntu 24.04, configure it with
  `-DCMAKE_CXX_FLAGS="-U_FORTIFY_SOURCE -D_FORTIFY_SOURCE=0"`, because
  `DensifyPointCloud` otherwise aborts with "buffer overflow detected".

## Taking the photos

- Take 20 to 100 sharp photos while walking round the part. Each photo should overlap the last by
  about two thirds. Go round twice, once low and once high.
- Put the part on a table with some texture, such as newspaper or a cutting mat. The camera solve
  needs detail to match between photos, and the table gives it plenty.
- Shiny, clear and plain single-colour parts reconstruct badly. A light dusting of developer
  spray, or masking tape, helps.
- **For scale**, stand a 1-2-3 block (or a 2-4-6 block, a gauge block, any box of known size, or
  a ball) on the table beside the part. Don't let it touch the part. rCAD finds it among the
  pieces on the table, scales the scan from it and leaves it out. Alternatively, lay a ruler
  beside the part and click two of its marks on a photo.

## The capture app

Photo folders from the Android capture app (`~/scans/<date>-<name>/`) have a `session.json` file.
rCAD reads:

- `reference`: the scale object, with `kind` set to `123-block`, `246-block`, `ruler`, `box` or
  `sphere`, and `dims_mm`. A block or ball is looked for automatically. A ruler's length becomes
  the default distance between the clicked marks.
- `calibration_photos`: the `CAL_*.jpg` photos, which show the reference. They take part in the
  reconstruction, and they are listed first when you click ruler marks.
- `complete`: a session that is still uploading isn't started.

The window's **Captures** menu lists the folders in `~/scans`.

## The run

The work files go in `rcad-scan/` inside the photo folder:

- `database.db`
- `sparse/`
- `dense/` (undistorted photos, depth maps and `mesh.ply`)
- `log.txt`
- `state.json`
- `part.ply` (the cleaned mesh at full detail)

The run does these steps in order:

1. Finding features
2. Matching photos (exhaustive up to 150 photos, sequential above that)
3. Solving cameras
4. Undistorting photos
5. Dense surface (two or three steps)
6. Meshing

**Stop** kills the running program. **Start** carries on from the last finished step, as long as
the photos and settings haven't changed. If only the surface method or the quality changed, the
camera solve is reused.

Quality sets the image size:

| quality | features at | dense step at | OpenMVS resolution level |
|---|---|---|---|
| Fast | 1600 px | 1200 px | 2 |
| Normal | 2400 px | 2000 px | 1 |
| High | 3200 px | 3200 px | 0 |

## Cleaning up and scale

- **Cut away the table:** rCAD finds the biggest flat area (the table) with RANSAC, then tightens
  the fit to the table's own noise. It keeps what is above the table: the biggest piece, plus any
  nearby pieces big enough to matter. The part's open bottom is where it stood on the table.
- **Stand the part on XY:** the table becomes Z = 0, the part is centred on the origin, and its
  longest footprint direction runs along X.
- **Thin the mesh:** quadric edge collapse reduces the mesh to about 40,000 triangles by default.
  This keeps the shape and the open edges.
- **Scale:**
  - **Reference block:** finds the block among the pieces on the table. Each piece's footprint
    rectangle and height are compared with the block's sizes, and a piece matches when all three
    agree within 6%.
  - **Ruler marks:** each of the two clicked marks is cast from that photo's camera onto the
    table plane (or onto the mesh). The scale is the distance you enter divided by the distance
    between the two hits.
  - **Number:** a scale you type in.
  - **Later:** leaves the mesh unscaled. Measure something you know, then set the Import's
    scale to known ÷ measured.

The scan becomes an Import feature: a surface body of triangles stored in the document. Its scale
is the Import's `scale` value, so you can edit it later.

## Agent tool

```json
{"tool": "scan_photos", "args": {"photos": "~/scans/2026-11-21_1530-bracket", "method": "openmvs", "quality": "normal", "scale": {"reference": [25.4, 50.8, 76.2]}}}
```

`scale` can also be a number (mm per unit), or `{"photo": "CAL_0003.jpg", "a": [x, y], "b": [x, y], "mm": 100}`
with the marks in the undistorted photo's pixels (`rcad-scan/dense/images`). If you leave
`scale` out, a capture session with a reference block is scaled from that block.
