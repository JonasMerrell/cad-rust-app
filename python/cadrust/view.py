"""Rendering shapes to images without a window.

    from cadrust.view import Camera, Item, render

    cam = Camera.iso()
    img = render([Item(shape, color=(0.8, 0.5, 0.2))], cam, 1400, 1050)
    img.save('part.png')

Shapes are tessellated once (`Item` caches a shape's mesh); each image gives every item its colour,
opacity, placement and edge lines. The camera is orthographic by default and is fitted to the items,
or to a box you give. `Camera.project` maps model points to pixels for labels drawn on top.
"""

import math

from . import _core as C

MESH_CACHE = {}


def _v(p):
    return (float(p[0]), float(p[1]), float(p[2]))


def _unit(a):
    n = math.sqrt(sum(c * c for c in a))
    return tuple(c / n for c in a)


def _cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def _dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def mesh_of(shape, linear=None, angular=0.14, edges=True):
    """The cached render mesh of a shape (a cadrust.freecad shape or a core Shape). The linear deflection
    defaults to 1/1000 of the shape's size, at least 0.02 mm."""
    core = getattr(shape, '_s', shape)
    key = (id(core), linear, angular, edges)
    hit = MESH_CACHE.get(key)
    if hit is not None and hit[0] is core:
        return hit[1]
    if linear is None:
        b = core.bbox(False)
        diag = math.dist(b[:3], b[3:]) if b else 1.0
        linear = max(0.02, diag * 1e-3)
    m = core.mesh(linear, angular, edges)
    MESH_CACHE[key] = (core, m)
    return m


class Item:
    """A shape in an image: colour (r, g, b) 0–1, opacity 0–1, placement (a 3 × 4 row-major rigid transform,
    an offset (x, y, z) or None), edge colour (None: no edge lines), edge width in pixels, shine 0–1."""

    def __init__(self, shape, color=(0.7, 0.72, 0.75), opacity=1.0, placement=None, edges=(0.12, 0.12, 0.12),
                 edge_width=2.0, shine=0.3, mesh=None):
        self.shape = shape
        self.mesh = mesh if mesh is not None else mesh_of(shape)
        self.color, self.opacity, self.edges, self.edge_width, self.shine = color, opacity, edges, edge_width, shine
        self.placement = placement

    def _tuple(self):
        pl = self.placement
        if pl is None:
            pl = (0.0, 0.0, 0.0)
        elif hasattr(pl, 'A'):                    # a cadrust.freecad Matrix
            pl = pl.A[:12]
        elif hasattr(pl, 'toMatrix'):             # a Placement
            pl = pl.toMatrix().A[:12]
        return (self.mesh, tuple(self.color), float(self.opacity), [float(v) for v in pl],
                tuple(self.edges) if self.edges is not None else None, float(self.edge_width), float(self.shine))

    def bbox(self):
        """The item's box after its placement (corners of the mesh box transformed)."""
        b = self.mesh.bbox()
        if b is None:
            return None
        pl = self._tuple()[3]
        if len(pl) == 3:
            pl = [1, 0, 0, pl[0], 0, 1, 0, pl[1], 0, 0, 1, pl[2]]
        pts = [(x, y, z) for x in (b[0], b[3]) for y in (b[1], b[4]) for z in (b[2], b[5])]
        out = [(pl[0] * x + pl[1] * y + pl[2] * z + pl[3], pl[4] * x + pl[5] * y + pl[6] * z + pl[7],
                pl[8] * x + pl[9] * y + pl[10] * z + pl[11]) for x, y, z in pts]
        return tuple(min(p[k] for p in out) for k in range(3)) + tuple(max(p[k] for p in out) for k in range(3))


class Camera:
    """An orthographic (or, with fov, perspective) camera looking along `direction` with `up` toward the top
    of the image. `target` and `height` are filled in by fitting unless given."""

    def __init__(self, direction, up=(0, 0, 1), target=None, height=None, fov=0.0):
        self.direction = _unit(_v(direction))
        up = _v(up)
        if abs(_dot(_unit(up), self.direction)) > 0.999:
            up = (0.0, 1.0, 0.0) if abs(self.direction[1]) < 0.9 else (1.0, 0.0, 0.0)
        self.right = _unit(_cross(self.direction, up))
        self.up = _cross(self.right, self.direction)
        self.target, self.height, self.fov = target, height, fov
        self.size = None

    # the standard views, as FreeCAD names them (the camera's line of sight)
    @classmethod
    def iso(cls):
        return cls((-1, 1, -1))

    @classmethod
    def rear_iso(cls):
        return cls((1, -1, -1))

    @classmethod
    def front(cls):
        return cls((0, 1, 0))

    @classmethod
    def rear(cls):
        return cls((0, -1, 0))

    @classmethod
    def right_side(cls):
        return cls((-1, 0, 0))

    @classmethod
    def left_side(cls):
        return cls((1, 0, 0))

    @classmethod
    def top(cls):
        return cls((0, 0, -1), up=(0, 1, 0))

    @classmethod
    def bottom(cls):
        return cls((0, 0, 1), up=(0, 1, 0))

    @classmethod
    def look(cls, direction, up=(0, 0, 1)):
        return cls(direction, up)

    def fit(self, box, aspect, margin=1.05):
        """Center on a box (xmin, ymin, zmin, xmax, ymax, zmax) and size the view to hold it."""
        pts = [(x, y, z) for x in (box[0], box[3]) for y in (box[1], box[4]) for z in (box[2], box[5])]
        us = [_dot(p, self.right) for p in pts]
        vs = [_dot(p, self.up) for p in pts]
        ws = [_dot(p, self.direction) for p in pts]
        cu, cv, cw = (max(us) + min(us)) / 2, (max(vs) + min(vs)) / 2, (max(ws) + min(ws)) / 2
        self.target = tuple(self.right[k] * cu + self.up[k] * cv + self.direction[k] * cw for k in range(3))
        self.height = max(max(vs) - min(vs), (max(us) - min(us)) / aspect) * margin
        self.depth = max(ws) - min(ws)
        return self

    def fit_diagonal(self, box, margin=1.05):
        """Center on a box and make the view as tall as its diagonal (FreeCAD's zoom to a selection)."""
        self.fit(box, 1.0)
        self.height = math.dist(box[:3], box[3:]) * margin
        return self

    def eye(self):
        far = 10.0 * (self.height or 1000.0) + 10000.0
        return tuple(self.target[k] - self.direction[k] * far for k in range(3))

    def project(self, p):
        """Pixel (x, y) of a model point in the last image rendered with this camera."""
        w, h = self.size
        d = tuple(_v(p)[k] - self.target[k] for k in range(3))
        x = _dot(d, self.right) / (self.height * 0.5 * w / h)
        y = _dot(d, self.up) / (self.height * 0.5)
        return ((x + 1) * 0.5 * w, (1 - y) * 0.5 * h)

    def scale(self):
        """Pixels per mm."""
        return self.size[1] / self.height


def union(boxes):
    boxes = [b for b in boxes if b is not None]
    if not boxes:
        return None
    return tuple(min(b[k] for b in boxes) for k in range(3)) + tuple(max(b[k] for b in boxes) for k in range(3, 6))


def render(items, camera, width, height, background=(1.0, 1.0, 1.0), fit=True, margin=1.05):
    """Render the items with the camera to a PIL image (or raw RGB bytes when Pillow is missing). With
    `fit`, a camera without a target is fitted to the items."""
    if fit and (camera.target is None or camera.height is None):
        box = union(it.bbox() for it in items)
        camera.fit(box if box else (0, 0, 0, 1, 1, 1), width / height, margin)
    camera.size = (width, height)
    px = C.render_meshes([it._tuple() for it in items], camera.eye(), camera.target, camera.up, width, height,
                         ortho_height=camera.height if not camera.fov else 0.0, fov=camera.fov or 30.0,
                         background=tuple(background))
    try:
        from PIL import Image
    except ImportError:
        return px
    return Image.frombytes('RGB', (width, height), px)
