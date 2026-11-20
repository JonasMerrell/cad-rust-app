#!/usr/bin/env python3
"""Carrier 2 of the sprung cycloidal drive: a 'spider'. Three stiff arms clamp the A sleeves; the three B sleeves sit on
pads carried by thin radial blades, which give the preload its spring. The outline is what a 6.35 mm end mill leaves
(every inside corner opened to R3.3)."""
import math
import numpy as np
from shapely.geometry import Point, Polygon, LineString
from shapely.ops import unary_union

def outline(RO=29.0, t=2.0, hub=12.0, bore=8.6, pad=7.5, arm=8.0, rim=38.0, cutter=3.3, SA=(0, 120, 240), SB=(60, 180, 300)):
    pt = lambda r, a: (r*math.cos(math.radians(a)), r*math.sin(math.radians(a)))
    shapes = [Point(0, 0).buffer(hub, 64)]
    for a in SA: shapes.append(LineString([(0, 0), pt(RO, a)]).buffer(arm, 32))
    for b in SB:
        shapes.append(LineString([pt(hub - 2, b), pt(RO, b)]).buffer(t/2, cap_style=2))
        shapes.append(Point(*pt(RO, b)).buffer(pad, 48))
    raw = unary_union(shapes)
    big = Point(0, 0).buffer(rim + 30, 64)
    void = big.difference(raw).buffer(-cutter, 24).buffer(cutter, 24)        # what the cutter can clear
    body = Point(0, 0).buffer(rim, 96).difference(void)
    if body.geom_type != "Polygon": body = max(body.geoms, key=lambda g: g.area)
    return body.simplify(0.01)

def blade_k(body, RO=29.0, b_axial=9.53, hub=12.0, ang=60.0, E=210e3, z_off=9.3, G=80e3):
    """Tangential stiffness at the B sleeve (N/mm): bending of the blade as a beam of varying width, plus its twist under the offset load."""
    c, s = math.cos(math.radians(ang)), math.sin(math.radians(ang)); comp = 0.0; tw = 0.0; ds = 0.05
    for r in np.arange(hub*0.6, RO, ds):
        line = LineString([(r*c + 20*s, r*s - 20*c), (r*c - 20*s, r*s + 20*c)]); x = body.intersection(line)
        segs = [x] if x.geom_type == "LineString" else list(getattr(x, "geoms", []))
        w = 0.0
        for g in segs:
            if g.distance(Point(r*c, r*s)) < 1e-6: w = g.length
        if w <= 0 or w > 14: continue
        I = b_axial*w**3/12; comp += (RO - r)**2/(E*I)*ds
        J = b_axial*w**3/3*(1 - 0.63*w/b_axial) if w < b_axial else w*b_axial**3/3*(1 - 0.63*b_axial/w)
        tw += ds/(G*J)
    return 1/(comp + tw*z_off**2), 1/comp, 1/(tw*z_off**2)

if __name__ == "__main__":
    for t in (1.6, 1.8, 2.0, 2.2, 2.5):
        for hub in (11.0, 12.0):
            body = outline(t=t, hub=hub)
            print(t, hub, [round(v) for v in blade_k(body, hub=hub)], round(body.area), len(body.exterior.coords), len(body.interiors))
