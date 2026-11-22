Drill scan model input (DeWalt DC742)
Built with rCAD d6dffa3 at 80.9 mm/unit, the mean of the two block-edge click scales (78.8 and 83.0).
Frame: the scan_photos result frame, in mm, with the drill standing on XY and Z up. The chuck points to -X (azimuth about 175 deg).

drill-scan-mesh-mm.stl
  Binary STL with 150,000 triangles. It is rcad-scan/part.ply from a rerun with triangles 150000
  (same cleanup, same transform), multiplied by 80.9.
  bbox [-159.70, -156.69, 1.14] .. [159.63, 156.86, 252.64] mm.
  The X/Y extent includes a few leftover floor patches; the drill itself is narrower.

drill-scan-points-mm.ply
  Binary PLY with 400,000 points (subsampled from 480,468): float xyz in mm, uchar rgb.
  These are the fused.ply points inside the mesh bbox padded by 10 mm and above Z = 3 mm, in the same frame.
  The transform was fitted by rigid ICP of the fused points onto part.ply (in units), then multiplied by 80.9.
  On the drill, the median point-to-mesh distance is 0.87 mm. The p90 is large (35 mm) because the padded box
  also keeps carpet and shadow points that the mesh cleanup removed.

Drill core extent, from the points above Z = 10 mm (0.5 to 99.5 percentiles, still with some floor noise):
  X -55.1 .. 141.2, Y -16.8 .. 91.2, Z 10.7 .. 245.5 mm (about 196 x 108 x 235). The battery is about 95 x 84.

Raw COLMAP (fused.ply) to part frame, in units: x_part = M @ x_raw + o, then multiply by 80.9 for mm.
M = [[ 0.18506818,  0.97248061, -0.14153175],
     [ 0.78880912, -0.23289663, -0.56880518],
     [-0.58611428, -0.0063738 , -0.81020333]]
o = [-0.51555702,  0.82304883,  4.0772613 ]

Reference photos are not in the repo: they show the room, so they stay on the machine for now, in
~/scans/model-input/ (1600 px long side, EXIF-rotated).
Camera positions in the part frame (azimuth from +X, elevation, distance in mm):
  IMG_0047_front-chuck-end.jpg                 az  176.8  el 37.2  455
  IMG_0016_back-motor-end.jpg                  az    1.7  el 55.9  494
  IMG_0030_side-plusY-label.jpg                az   83.7  el 61.3  507  (DC742 label side)
  IMG_0001_side-minusY-logo-rear-quarter.jpg   az  -65.5  el 55.7  440  (DEWALT logo side)
  IMG_0100_side-minusY-logo-front-quarter.jpg  az -137.0  el 44.1  363
  IMG_0060_top-oblique-plusY.jpg               az  108.6  el 67.8  548  (highest shot)
No camera sits between azimuth -129 and -65, so there is no square-on -Y side view; the two -Y quarter views bracket it.
