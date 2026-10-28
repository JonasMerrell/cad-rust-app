"""Interactive web pages of a model: parts on a tree of nodes that slide or turn within limits, moved by sliders,
by dragging parts and by scripted tours, with layers to ghost or hide, a section plane, markers that read out
and trace a point, and wire boxes. The page is one .html file that opens in any browser (three.js comes from
a CDN, so it needs an internet connection).

    from cadrust.web import Scene
    sc = Scene('Slide')
    base = sc.node('Base')
    car = sc.node('Carriage', parent=base, slider=(1, 0, 0), limits=(-50, 50), label='X')
    sc.part(base_shape, base, color=(0.5, 0.5, 0.52), layer='Castings')
    sc.part(carriage_shape, car, color=(0.8, 0.8, 0.84))
    sc.tour('Back and forth', [({car: -50}, 20), ({car: 50}, 20)])
    sc.write('slide.html')

Shapes are drawn as they are (the node's parts at the joint's home value); positions, axes and origins are in
the parent node's frame, which is the world frame for nodes without a `base` placement.
"""

import json

from . import _core


def _core_shape(shape):
    return getattr(shape, '_s', shape)


def _rows(m):
    """A 3 × 4 row-major list from a cadrust.freecad Matrix, a 4 × 4 / 3 × 4 nested list or a flat list."""
    if m is None:
        return None
    if hasattr(m, 'A'):
        m = m.A
    flat = [float(v) for row in m for v in row] if isinstance(m[0], (list, tuple)) else [float(v) for v in m]
    return flat[:12]


class Scene:
    def __init__(self, title, notes=None, linear=0.25, angular=0.35):
        self.title, self.notes = title, notes
        self.linear, self.angular = linear, angular
        self.nodes, self.parts, self.morphs, self.markers, self.boxes, self.tours = [], [], [], [], [], []
        self.layers = []
        self.meshes = []

    # ------------------------------------------------------------ structure
    def node(self, name, parent=None, slider=None, revolute=None, origin=(0, 0, 0), limits=None, home=0.0,
             value=None, label=None, follows=None, ratio=1.0, offset=0.0, base=None, explode=None):
        """A node; returns its number. slider=axis (mm along it) or revolute=axis (degrees about it, through
        `origin`). follows=node: this joint's value is ratio × that node's + offset (no slider of its own)."""
        n = dict(name=name)
        if parent is not None:
            n['parent'] = parent
        if base is not None:
            n['base'] = _rows(base)
        axis = slider if slider is not None else revolute
        if axis is not None:
            j = dict(kind='slider' if slider is not None else 'revolute', axis=[float(v) for v in axis],
                     origin=[float(v) for v in origin], home=float(home))
            if limits is not None:
                j['limits'] = [float(limits[0]), float(limits[1])]
            if value is not None:
                j['value'] = float(value)
            if label:
                j['label'] = label
            if follows is not None:
                j['drive'] = dict(of=follows, ratio=float(ratio), offset=float(offset))
            n['joint'] = j
        if explode is not None:
            n['explode'] = [float(v) for v in explode]
        self.nodes.append(n)
        return len(self.nodes) - 1

    def mesh(self, shape, edges=True, linear=None, angular=None):
        """Tessellate a shape once (to share between parts); returns its number."""
        self.meshes.append(_core.web_mesh(_core_shape(shape), linear or self.linear, angular or self.angular, edges))
        return len(self.meshes) - 1

    def part(self, shape, node, name=None, color=(0.7, 0.72, 0.76), opacity=1.0, shine=0.3, edges=True, layer=None,
             info=None, mesh=None, local=None):
        """A part on a node: a shape, or `mesh` (a number from mesh()) placed by `local` (3 × 4)."""
        if mesh is None:
            mesh = self.mesh(shape, edges)
        p = dict(name=name or f'Part {len(self.parts) + 1}', node=node, mesh=mesh, color=[float(c) for c in color],
                 opacity=float(opacity), shine=float(shine), edges=bool(edges))
        if layer:
            p['layer'] = layer
        if info:
            p['info'] = [[str(k), str(v)] for k, v in (info.items() if isinstance(info, dict) else info)]
        if local is not None:
            p['local'] = _rows(local)
        self.parts.append(p)
        return len(self.parts) - 1

    def morph(self, name, node, drivers, grid, shapes, color=(0.7, 0.72, 0.76), opacity=1.0, shine=0.3, layer=None,
              linear=None, angular=None):
        """A part whose shape follows joint values: `shapes` row-major over `grid` (one list of values per
        driver node, the last fastest), None where it is absent. Samples with the same triangles are blended."""
        frames = [None if s is None else self.mesh(s, True, linear, angular) for s in shapes]
        m = dict(name=name, node=node, drivers=list(drivers), grid=[[float(v) for v in g] for g in grid], frames=frames,
                 color=[float(c) for c in color], opacity=float(opacity), shine=float(shine))
        if layer:
            m['layer'] = layer
        self.morphs.append(m)

    def marker(self, name, node, pos, relative=None, trail=False, axes=None):
        """A point on `node`, read out in `relative`'s frame from where it is at home; trail traces its path."""
        mk = dict(name=name, node=node, pos=[float(v) for v in pos], trail=bool(trail))
        if relative is not None:
            mk['relative'] = relative
        if axes:
            mk['axes'] = list(axes)
        self.markers.append(mk)

    def box(self, name, node, lo, hi):
        self.boxes.append(dict(name=name, node=node, lo=[float(v) for v in lo], hi=[float(v) for v in hi]))

    def tour(self, name, steps):
        """steps: ({node: value}, feed mm/s or deg/s[, note]) or dicts {values, feed | time, note}."""
        out = []
        for s in steps:
            if isinstance(s, dict) and 'values' in s:
                st = dict(s)
                st['values'] = [[k, float(v)] for k, v in s['values'].items()]
            else:
                values, feed, *rest = s
                st = dict(values=[[k, float(v)] for k, v in values.items()], feed=float(feed))
                if rest and rest[0]:
                    st['note'] = rest[0]
            out.append(st)
        self.tours.append(dict(name=name, steps=out))

    def layer(self, name, state):
        """How a layer starts out: 'show', 'ghost' or 'hide' (the viewer can change it)."""
        self.layers.append([name, state])

    # ------------------------------------------------------------ output
    def spec(self):
        s = dict(title=self.title, nodes=self.nodes, parts=self.parts, morphs=self.morphs, markers=self.markers,
                 boxes=self.boxes, tours=self.tours, layers=self.layers)
        if self.notes:
            s['notes'] = self.notes
        return s

    def write(self, path, inline=True):
        """Write the page (.html). inline=False puts the meshes in a .json and .bin beside it, which the page
        fetches, so it has to be served over http. Returns (bytes, meshes, triangles)."""
        return _core.web_write(json.dumps(self.spec()), self.meshes, str(path), inline)
