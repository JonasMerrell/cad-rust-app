"""A FreeCAD-style API on cad-rust-app's OpenCascade kernel.

Covers the parts of FreeCAD's `FreeCAD` (Vector, Matrix, Rotation, Placement, BoundBox) and `Part`
modules that parametric modelling scripts use, with FreeCAD's semantics:

    from cadrust.freecad import App, Part, Vector, Matrix

    box = Part.makeBox(10, 20, 30)
    hole = Part.makeCylinder(3, 40, Vector(5, 10, -5))
    part = box.cut(hole).removeSplitter()
    part.Volume, part.BoundBox.ZLength

Shapes are values as in FreeCAD: operations return new shapes, while translate, rotate and
transformShape move the shape in place. There is no document or GUI: `App.GuiUp` is False.
"""

import math
import types

from . import _core as C
from ._core import OCCError

__all__ = ['App', 'Part', 'Vector', 'Matrix', 'Rotation', 'Placement', 'BoundBox', 'OCCError']

EPS = 1e-12


# --------------------------------------------------------------------------------------- Vector
class Vector:
    """A 3D vector (FreeCAD.Vector): mutable, with `*` between vectors giving the dot product."""

    __slots__ = ('x', 'y', 'z')

    def __init__(self, x=0.0, y=0.0, z=0.0):
        if isinstance(x, (Vector, tuple, list)):
            x, y, z = x
        self.x, self.y, self.z = float(x), float(y), float(z)

    # sequence protocol
    def __iter__(self):
        return iter((self.x, self.y, self.z))

    def __len__(self):
        return 3

    def __getitem__(self, i):
        return (self.x, self.y, self.z)[i]

    def __setitem__(self, i, v):
        setattr(self, 'xyz'[i], float(v))

    def __repr__(self):
        return f'Vector ({self.x!r}, {self.y!r}, {self.z!r})'

    def __eq__(self, o):
        return isinstance(o, Vector) and self.x == o.x and self.y == o.y and self.z == o.z

    def __ne__(self, o):
        return not self == o

    __hash__ = None

    # arithmetic
    def __add__(self, o):
        return Vector(self.x + o.x, self.y + o.y, self.z + o.z)

    def __sub__(self, o):
        return Vector(self.x - o.x, self.y - o.y, self.z - o.z)

    def __neg__(self):
        return Vector(-self.x, -self.y, -self.z)

    def __pos__(self):
        return Vector(self)

    def __mul__(self, o):
        if isinstance(o, Vector):
            return self.x * o.x + self.y * o.y + self.z * o.z
        return Vector(self.x * o, self.y * o, self.z * o)

    def __rmul__(self, o):
        return Vector(self.x * o, self.y * o, self.z * o)

    def __truediv__(self, o):
        return Vector(self.x / o, self.y / o, self.z / o)

    def __mod__(self, o):
        return self.cross(o)

    def add(self, o):
        return self + o

    def sub(self, o):
        return self - o

    def negative(self):
        return -self

    def dot(self, o):
        return self.x * o.x + self.y * o.y + self.z * o.z

    def cross(self, o):
        return Vector(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)

    def multiply(self, f):
        """Scale in place (returns self)."""
        self.x *= f
        self.y *= f
        self.z *= f
        return self

    def scale(self, fx, fy, fz):
        self.x *= fx
        self.y *= fy
        self.z *= fz
        return self

    @property
    def Length(self):
        return math.sqrt(self.x * self.x + self.y * self.y + self.z * self.z)

    @Length.setter
    def Length(self, value):
        n = self.Length
        if n < EPS:
            raise ValueError('Cannot set the length of a null vector')
        self.multiply(value / n)

    def normalize(self):
        """Make unit length in place (returns self)."""
        n = self.Length
        if n < EPS:
            raise ValueError('Cannot normalize null vector')
        return self.multiply(1.0 / n)

    def isEqual(self, o, tol=0.0):
        return (self - o).Length <= tol

    def isParallel(self, o, tol=1e-9):
        return self.cross(o).Length <= tol * max(self.Length * o.Length, EPS)

    def getAngle(self, o):
        """Angle to another vector (radians)."""
        d = self.Length * o.Length
        if d < EPS:
            return math.nan
        return math.acos(max(-1.0, min(1.0, self.dot(o) / d)))

    def distanceToPoint(self, p):
        return (self - p).Length

    def distanceToLine(self, base, d):
        u = Vector(d).normalize()
        w = self - base
        return (w - u * w.dot(u)).Length

    def distanceToPlane(self, base, n):
        u = Vector(n).normalize()
        return (self - base).dot(u)

    def projectToPlane(self, base, n):
        """Project onto the plane in place (returns self)."""
        u = Vector(n).normalize()
        d = (self - base).dot(u)
        self.x -= u.x * d
        self.y -= u.y * d
        self.z -= u.z * d
        return self

    def projectToLine(self, base, d):
        """The perpendicular from this point to the line (FreeCAD's odd convention: in place)."""
        u = Vector(d).normalize()
        w = self - base
        perp = u * w.dot(u) - w
        self.x, self.y, self.z = perp.x, perp.y, perp.z
        return self


def _v(p):
    """A point or direction as a float triple."""
    if isinstance(p, Vector):
        return (p.x, p.y, p.z)
    x, y, z = p
    return (float(x), float(y), float(z))


# --------------------------------------------------------------------------------------- Matrix
class Matrix:
    """A 4 × 4 matrix (FreeCAD.Matrix), row-major: Matrix(a11, a12, …, a44)."""

    def __init__(self, *args):
        if len(args) == 1 and isinstance(args[0], Matrix):
            self.A = list(args[0].A)
        elif len(args) == 0:
            self.A = [1.0, 0, 0, 0, 0, 1.0, 0, 0, 0, 0, 1.0, 0, 0, 0, 0, 1.0]
        elif len(args) <= 16:
            a = [float(v) for v in args] + [0.0] * (16 - len(args))
            if len(args) < 16:
                a[15] = 1.0
            self.A = a
        else:
            raise TypeError('Matrix takes up to 16 values')

    def _get(self, i, j):
        return self.A[4 * i + j]

    def __repr__(self):
        rows = ', '.join('(' + ', '.join(repr(self.A[4 * i + j]) for j in range(4)) + ')' for i in range(4))
        return f'Matrix ({rows})'

    def __eq__(self, o):
        return isinstance(o, Matrix) and self.A == o.A

    __hash__ = None

    def multiply(self, o):
        """Matrix × Matrix (a new matrix) or Matrix × Vector (the transformed point)."""
        if isinstance(o, Matrix):
            a, b = self.A, o.A
            return Matrix(*[sum(a[4 * i + k] * b[4 * k + j] for k in range(4)) for i in range(4) for j in range(4)])
        return self.multVec(o)

    __mul__ = multiply

    def multVec(self, v):
        a = self.A
        x, y, z = _v(v)
        return Vector(a[0] * x + a[1] * y + a[2] * z + a[3], a[4] * x + a[5] * y + a[6] * z + a[7],
                      a[8] * x + a[9] * y + a[10] * z + a[11])

    def move(self, v):
        """Add a translation in place."""
        x, y, z = _v(v)
        self.A[3] += x
        self.A[7] += y
        self.A[11] += z

    def scale(self, *f):
        fx, fy, fz = (f[0], f[0], f[0]) if len(f) == 1 and not isinstance(f[0], Vector) else _v(f[0] if len(f) == 1 else f)
        m = Matrix(fx, 0, 0, 0, 0, fy, 0, 0, 0, 0, fz, 0, 0, 0, 0, 1)
        self.A = m.multiply(self).A

    def _rot(self, m):
        self.A = m.multiply(self).A

    def rotateX(self, a):
        c, s = math.cos(a), math.sin(a)
        self._rot(Matrix(1, 0, 0, 0, 0, c, -s, 0, 0, s, c, 0, 0, 0, 0, 1))

    def rotateY(self, a):
        c, s = math.cos(a), math.sin(a)
        self._rot(Matrix(c, 0, s, 0, 0, 1, 0, 0, -s, 0, c, 0, 0, 0, 0, 1))

    def rotateZ(self, a):
        c, s = math.cos(a), math.sin(a)
        self._rot(Matrix(c, -s, 0, 0, s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1))

    def determinant(self):
        a = self.A
        return (a[0] * (a[5] * a[10] - a[6] * a[9]) - a[1] * (a[4] * a[10] - a[6] * a[8])
                + a[2] * (a[4] * a[9] - a[5] * a[8]))

    def transposed(self):
        return Matrix(*[self.A[4 * j + i] for i in range(4) for j in range(4)])

    def inverse(self):
        """Inverse of an affine matrix."""
        a = self.A
        det = self.determinant()
        if abs(det) < 1e-300:
            raise ValueError('Cannot invert singular matrix')
        r = [[(a[5] * a[10] - a[6] * a[9]) / det, (a[2] * a[9] - a[1] * a[10]) / det, (a[1] * a[6] - a[2] * a[5]) / det],
             [(a[6] * a[8] - a[4] * a[10]) / det, (a[0] * a[10] - a[2] * a[8]) / det, (a[2] * a[4] - a[0] * a[6]) / det],
             [(a[4] * a[9] - a[5] * a[8]) / det, (a[1] * a[8] - a[0] * a[9]) / det, (a[0] * a[5] - a[1] * a[4]) / det]]
        t = (a[3], a[7], a[11])
        out = []
        for i in range(3):
            out += r[i] + [-sum(r[i][k] * t[k] for k in range(3))]
        return Matrix(*out, 0, 0, 0, 1)

    def hasScale(self, tol=1e-7):
        """0 when the 3 × 3 part is a rotation (FreeCAD returns a ScaleType; non-zero means scaled)."""
        a = self.A
        cols = [(a[j], a[4 + j], a[8 + j]) for j in range(3)]
        ok = all(abs(math.sqrt(sum(c * c for c in col)) - 1.0) <= tol for col in cols)
        return 0 if ok else 2

    def isOrthogonal(self, tol=1e-6):
        return self.hasScale(tol) == 0

    def _m34(self):
        return [self.A[i] for i in range(12)]


for _i in range(4):
    for _j in range(4):
        def _acc(i=_i, j=_j):
            return property(lambda self: self.A[4 * i + j], lambda self, v: self.A.__setitem__(4 * i + j, float(v)))
        setattr(Matrix, f'A{_i + 1}{_j + 1}', _acc())


# --------------------------------------------------------------------------------------- Rotation
class Rotation:
    """A rotation (FreeCAD.Rotation) as a unit quaternion Q = (x, y, z, w).

    Rotation(axis, degrees), Rotation(from_vector, to_vector), Rotation(Matrix), Rotation(x, y, z, w),
    Rotation(yaw, pitch, roll) in degrees.
    """

    def __init__(self, *args):
        self.Q = (0.0, 0.0, 0.0, 1.0)
        if not args:
            return
        if len(args) == 1 and isinstance(args[0], Rotation):
            self.Q = args[0].Q
        elif len(args) == 1 and isinstance(args[0], Matrix):
            self._from_matrix(args[0])
        elif len(args) == 2 and isinstance(args[1], Vector):
            self._from_to(Vector(args[0]), Vector(args[1]))
        elif len(args) == 2:
            self._axis_angle(Vector(args[0]), math.radians(args[1]))
        elif len(args) == 3:
            yaw, pitch, roll = (math.radians(a) for a in args)
            r = (Rotation(Vector(0, 0, 1), math.degrees(yaw)).multiply(Rotation(Vector(0, 1, 0), math.degrees(pitch)))
                 .multiply(Rotation(Vector(1, 0, 0), math.degrees(roll))))
            self.Q = r.Q
        elif len(args) == 4:
            self._set(*args)
        else:
            raise TypeError('Rotation: give (axis, angle), (from, to), a Matrix, a quaternion or yaw, pitch, roll')

    def _set(self, x, y, z, w):
        n = math.sqrt(x * x + y * y + z * z + w * w)
        if n < EPS:
            raise ValueError('Rotation: null quaternion')
        self.Q = (x / n, y / n, z / n, w / n)

    def _axis_angle(self, axis, angle):
        n = axis.Length
        if n < EPS:
            self.Q = (0.0, 0.0, 0.0, 1.0)
            return
        s = math.sin(angle / 2) / n
        self._set(axis.x * s, axis.y * s, axis.z * s, math.cos(angle / 2))

    def _from_to(self, u, v):
        u, v = Vector(u).normalize(), Vector(v).normalize()
        dot = u.dot(v)
        w = u.cross(v)
        if w.Length == 0.0:
            if dot > 0:
                self.Q = (0.0, 0.0, 0.0, 1.0)
            else:
                t = u.cross(Vector(1, 0, 0))
                if t.Length < 2.220446049250313e-16:
                    t = u.cross(Vector(0, 1, 0))
                self._set(t.x, t.y, t.z, 0.0)
        else:
            self._axis_angle(w, math.acos(max(-1.0, min(1.0, dot))))

    def _from_matrix(self, m):
        a = m.A
        r = [[a[0], a[1], a[2]], [a[4], a[5], a[6]], [a[8], a[9], a[10]]]
        # unscale the columns
        for j in range(3):
            n = math.sqrt(sum(r[i][j] ** 2 for i in range(3)))
            if n > EPS:
                for i in range(3):
                    r[i][j] /= n
        tr = r[0][0] + r[1][1] + r[2][2]
        if tr > 0:
            s = math.sqrt(tr + 1.0) * 2
            q = ((r[2][1] - r[1][2]) / s, (r[0][2] - r[2][0]) / s, (r[1][0] - r[0][1]) / s, 0.25 * s)
        elif r[0][0] > r[1][1] and r[0][0] > r[2][2]:
            s = math.sqrt(1.0 + r[0][0] - r[1][1] - r[2][2]) * 2
            q = (0.25 * s, (r[0][1] + r[1][0]) / s, (r[0][2] + r[2][0]) / s, (r[2][1] - r[1][2]) / s)
        elif r[1][1] > r[2][2]:
            s = math.sqrt(1.0 + r[1][1] - r[0][0] - r[2][2]) * 2
            q = ((r[0][1] + r[1][0]) / s, 0.25 * s, (r[1][2] + r[2][1]) / s, (r[0][2] - r[2][0]) / s)
        else:
            s = math.sqrt(1.0 + r[2][2] - r[0][0] - r[1][1]) * 2
            q = ((r[0][2] + r[2][0]) / s, (r[1][2] + r[2][1]) / s, 0.25 * s, (r[1][0] - r[0][1]) / s)
        self._set(*q)

    @property
    def Angle(self):
        """Angle in radians, in [0, 2π]."""
        return 2.0 * math.acos(max(-1.0, min(1.0, self.Q[3])))

    @property
    def Axis(self):
        a = self.Angle
        if abs(a) < 1e-12:
            return Vector(0, 0, 1)
        s = math.sin(a / 2)
        return Vector(self.Q[0] / s, self.Q[1] / s, self.Q[2] / s)

    RawAxis = Axis

    def __repr__(self):
        return f'Rotation ({self.Q[0]!r}, {self.Q[1]!r}, {self.Q[2]!r}, {self.Q[3]!r})'

    def multiply(self, o):
        """self × o: o applied first."""
        x1, y1, z1, w1 = self.Q
        x2, y2, z2, w2 = o.Q
        r = Rotation()
        r._set(w1 * x2 + x1 * w2 + y1 * z2 - z1 * y2, w1 * y2 - x1 * z2 + y1 * w2 + z1 * x2,
               w1 * z2 + x1 * y2 - y1 * x2 + z1 * w2, w1 * w2 - x1 * x2 - y1 * y2 - z1 * z2)
        return r

    __mul__ = multiply

    def inverted(self):
        x, y, z, w = self.Q
        return Rotation(-x, -y, -z, w)

    def invert(self):
        self.Q = self.inverted().Q

    def toMatrix(self):
        x, y, z, w = self.Q
        return Matrix(1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w), 0,
                      2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w), 0,
                      2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y), 0,
                      0, 0, 0, 1)

    def multVec(self, v):
        return self.toMatrix().multVec(v)

    def isIdentity(self, tol=0.0):
        return abs(self.Angle) <= tol

    def isNull(self):
        return self.Q == (0.0, 0.0, 0.0, 1.0)


# --------------------------------------------------------------------------------------- Placement
class Placement:
    """Position and orientation (FreeCAD.Placement): x ↦ Rotation·x + Base."""

    def __init__(self, *args):
        self.Base, self.Rotation = Vector(), Rotation()
        if len(args) == 1 and isinstance(args[0], Placement):
            self.Base, self.Rotation = Vector(args[0].Base), Rotation(args[0].Rotation)
        elif len(args) == 1 and isinstance(args[0], Matrix):
            m = args[0]
            self.Base, self.Rotation = Vector(m.A[3], m.A[7], m.A[11]), Rotation(m)
        elif len(args) == 2:
            base, rot = args
            self.Base = Vector(base)
            self.Rotation = rot if isinstance(rot, Rotation) else Rotation(*rot)
        elif len(args) == 3 and isinstance(args[1], Rotation):
            base, rot, center = args       # rotation about `center`, then the move
            self.Rotation = rot
            self.Base = Vector(base) + Vector(center) - rot.multVec(center)
        elif len(args) == 3:
            base, axis, angle = args
            self.Base, self.Rotation = Vector(base), Rotation(axis, angle)
        elif args:
            raise TypeError('Placement: give (base, rotation), (base, axis, angle) or a Matrix')

    def toMatrix(self):
        m = self.Rotation.toMatrix()
        m.A[3], m.A[7], m.A[11] = self.Base.x, self.Base.y, self.Base.z
        return m

    @property
    def Matrix(self):
        return self.toMatrix()

    def multiply(self, o):
        return Placement(self.toMatrix().multiply(o.toMatrix()))

    __mul__ = multiply

    def multVec(self, v):
        return self.toMatrix().multVec(v)

    def inverse(self):
        return Placement(self.toMatrix().inverse())

    def isIdentity(self):
        return self.Base.Length == 0 and self.Rotation.isNull()

    def __repr__(self):
        return f'Placement [Pos={tuple(self.Base)}, Rot={self.Rotation!r}]'


# --------------------------------------------------------------------------------------- BoundBox
class BoundBox:
    """Axis-aligned box (FreeCAD.BoundBox). BoundBox() is empty (invalid) until something is added."""

    def __init__(self, *args):
        big = 1.7976931348623157e308
        self.XMin = self.YMin = self.ZMin = big
        self.XMax = self.YMax = self.ZMax = -big
        if len(args) == 1 and isinstance(args[0], BoundBox):
            b = args[0]
            self.XMin, self.YMin, self.ZMin, self.XMax, self.YMax, self.ZMax = b.XMin, b.YMin, b.ZMin, b.XMax, b.YMax, b.ZMax
        elif len(args) == 2:
            a, b = Vector(args[0]), Vector(args[1])
            self.XMin, self.YMin, self.ZMin = min(a.x, b.x), min(a.y, b.y), min(a.z, b.z)
            self.XMax, self.YMax, self.ZMax = max(a.x, b.x), max(a.y, b.y), max(a.z, b.z)
        elif len(args) == 6:
            self.XMin, self.YMin, self.ZMin, self.XMax, self.YMax, self.ZMax = (float(v) for v in args)
        elif args:
            raise TypeError('BoundBox: give a BoundBox, two corners or six values')

    def isValid(self):
        return self.XMin <= self.XMax and self.YMin <= self.YMax and self.ZMin <= self.ZMax

    def add(self, o):
        if isinstance(o, BoundBox):
            if not o.isValid():
                return
            pts = [(o.XMin, o.YMin, o.ZMin), (o.XMax, o.YMax, o.ZMax)]
        else:
            pts = [_v(o)]
        for x, y, z in pts:
            self.XMin, self.YMin, self.ZMin = min(self.XMin, x), min(self.YMin, y), min(self.ZMin, z)
            self.XMax, self.YMax, self.ZMax = max(self.XMax, x), max(self.YMax, y), max(self.ZMax, z)

    def intersect(self, o, *rest):
        """Whether two boxes overlap (touching counts)."""
        if rest:
            raise NotImplementedError('BoundBox.intersect(base, dir) is not supported')
        return not (o.XMax < self.XMin or o.XMin > self.XMax or o.YMax < self.YMin or o.YMin > self.YMax
                    or o.ZMax < self.ZMin or o.ZMin > self.ZMax)

    def intersected(self, o):
        return BoundBox(max(self.XMin, o.XMin), max(self.YMin, o.YMin), max(self.ZMin, o.ZMin),
                        min(self.XMax, o.XMax), min(self.YMax, o.YMax), min(self.ZMax, o.ZMax))

    getIntersectedBoundBox = intersected

    def united(self, o):
        b = BoundBox(self)
        b.add(o)
        return b

    def enlarge(self, d):
        self.XMin -= d
        self.YMin -= d
        self.ZMin -= d
        self.XMax += d
        self.YMax += d
        self.ZMax += d

    def move(self, v):
        x, y, z = _v(v)
        self.XMin += x
        self.XMax += x
        self.YMin += y
        self.YMax += y
        self.ZMin += z
        self.ZMax += z

    def isInside(self, o):
        if isinstance(o, BoundBox):
            return (self.XMin <= o.XMin and o.XMax <= self.XMax and self.YMin <= o.YMin and o.YMax <= self.YMax
                    and self.ZMin <= o.ZMin and o.ZMax <= self.ZMax)
        x, y, z = _v(o)
        return self.XMin <= x <= self.XMax and self.YMin <= y <= self.YMax and self.ZMin <= z <= self.ZMax

    @property
    def XLength(self):
        return self.XMax - self.XMin

    @property
    def YLength(self):
        return self.YMax - self.YMin

    @property
    def ZLength(self):
        return self.ZMax - self.ZMin

    @property
    def Center(self):
        return Vector((self.XMin + self.XMax) / 2, (self.YMin + self.YMax) / 2, (self.ZMin + self.ZMax) / 2)

    @property
    def DiagonalLength(self):
        return math.sqrt(self.XLength ** 2 + self.YLength ** 2 + self.ZLength ** 2)

    def getPoint(self, i):
        return Vector((self.XMin, self.XMax)[i & 1], (self.YMin, self.YMax)[(i >> 1) & 1], (self.ZMin, self.ZMax)[(i >> 2) & 1])

    def __repr__(self):
        return f'BoundBox ({self.XMin}, {self.YMin}, {self.ZMin}, {self.XMax}, {self.YMax}, {self.ZMax})'


# --------------------------------------------------------------------------------------- transforms
def _translation(v):
    x, y, z = _v(v)
    return [1.0, 0, 0, x, 0, 1.0, 0, y, 0, 0, 1.0, z]


def _rotation(base, axis, degrees):
    """3 × 4 matrix of a rotation by `degrees` about the axis through base."""
    m = Rotation(Vector(axis), degrees).toMatrix()
    b = Vector(base)
    t = b - m.multVec(b)
    a = m.A
    return [a[0], a[1], a[2], t.x, a[4], a[5], a[6], t.y, a[8], a[9], a[10], t.z]


# --------------------------------------------------------------------------------------- shapes
_TYPE_NAMES = ('Compound', 'CompSolid', 'Solid', 'Shell', 'Face', 'Wire', 'Edge', 'Vertex')


def _wrap(cs):
    """A Python shape of the right class (Solid, Face, Edge, …) around a core shape."""
    t = cs.shape_type()
    cls = _CLASSES[t] if t >= 0 else Shape
    obj = object.__new__(cls)
    obj._s = cs
    obj._cache = {}
    return obj


def _core(s):
    if isinstance(s, Shape):
        return s._s
    raise TypeError(f'expected a shape, got {type(s).__name__}')


def _cores(shapes):
    if isinstance(shapes, Shape):
        return [shapes._s]
    return [_core(s) for s in shapes]


class Shape:
    """A shape (Part.Shape). Part.Shape() is a null shape."""

    def __init__(self, *args):
        if args:
            raise TypeError('Part.Shape() takes no arguments here; build shapes with the Part functions')
        self._s = C.Shape()
        self._cache = {}

    # -- type and validity
    @property
    def ShapeType(self):
        t = self._s.shape_type()
        return _TYPE_NAMES[t] if t >= 0 else 'Shape'

    def isNull(self):
        return self._s.is_null()

    def isValid(self):
        return not self._s.is_null() and self._s.is_valid()

    def isSame(self, o):
        return self._s.is_same(o._s)

    def isEqual(self, o):
        return self._s.is_same(o._s)

    def __repr__(self):
        return f'<{self.ShapeType} object>'

    def _cached(self, key, fn):
        c = self._cache
        if key not in c:
            c[key] = fn()
        return c[key]

    def _set(self, cs):
        self._s = cs
        self._cache = {}

    # -- sub-shapes
    def _subs(self, kind):
        return self._cached(('subs', kind), lambda: [_wrap(s) for s in self._s.subshapes(kind)])

    @property
    def Solids(self):
        return list(self._subs(0))

    @property
    def Shells(self):
        return list(self._subs(1))

    @property
    def Faces(self):
        return list(self._subs(2))

    @property
    def Wires(self):
        return list(self._subs(3))

    @property
    def Edges(self):
        return list(self._subs(4))

    @property
    def Vertexes(self):
        return list(self._subs(5))

    def childShapes(self, cumOri=True, cumLoc=True):
        return [_wrap(s) for s in self._s.children()]

    # -- measures
    @property
    def BoundBox(self):
        b = self._cached('bbox', lambda: self._s.bbox(False))
        return BoundBox() if b is None else BoundBox(*b)

    @property
    def Volume(self):
        return self._cached('volume', lambda: self._s.props(0)[0])

    @property
    def Area(self):
        return self._cached('area', lambda: self._s.props(1)[0])

    @property
    def Length(self):
        return self._cached('length', lambda: self._s.props(2)[0])

    @property
    def CenterOfMass(self):
        def com():
            if self._s.count(0):
                return self._s.props(0)[1]
            if self._s.count(2):
                return self._s.props(1)[1]
            if self._s.count(4):
                return self._s.props(2)[1]
            return self.Vertexes[0].Point if self.Vertexes else (0.0, 0.0, 0.0)
        return Vector(self._cached('com', com))

    @property
    def MatrixOfInertia(self):
        i = self._s.inertia()
        return Matrix(i[0], i[1], i[2], 0, i[3], i[4], i[5], 0, i[6], i[7], i[8], 0, 0, 0, 0, 1)

    # -- booleans
    def _bool(self, op, tools, tol=0.0):
        return _wrap(self._s.boolean(op, _cores(tools), float(tol)))

    def fuse(self, tools, tol=0.0):
        return self._bool(0, tools, tol)

    def multiFuse(self, tools, tol=0.0):
        return self._bool(0, tools, tol)

    def cut(self, tools, tol=0.0):
        return self._bool(1, tools, tol)

    def common(self, tools, tol=0.0):
        return self._bool(2, tools, tol)

    def removeSplitter(self):
        return _wrap(self._s.unify())

    # -- features
    def extrude(self, v):
        return _wrap(self._s.extrude(_v(v)))

    def revolve(self, base, axis, angle=360.0):
        return _wrap(self._s.revolve(_v(base), _v(axis), float(angle)))

    def makeFillet(self, *args):
        """makeFillet(radius, edges) or makeFillet(radius1, radius2, edges)."""
        if len(args) == 2:
            r1 = r2 = float(args[0])
            edges = args[1]
        else:
            r1, r2, edges = float(args[0]), float(args[1]), args[2]
        return _wrap(self._s.fillet(_cores(edges), r1, r2))

    def makeChamfer(self, *args):
        """makeChamfer(distance, edges)."""
        if len(args) != 2:
            raise NotImplementedError('makeChamfer: only equal distances (d, edges) are supported')
        return _wrap(self._s.chamfer(_cores(args[1]), float(args[0])))

    def makeOffset2D(self, offset, join=0, fill=False, openResult=False, intersection=False):
        """2D offset of a planar face (join 0: arcs, 2: sharp corners)."""
        if fill or openResult:
            raise NotImplementedError('makeOffset2D: fill and openResult are not supported')
        out = self._s.offset2d(float(offset), join == 0)
        kids = out.children()
        return _wrap(kids[0]) if len(kids) == 1 else _wrap(out)

    # -- placement
    def translate(self, v):
        self._set(self._s.moved(_translation(v)))
        return self

    def translated(self, v):
        return _wrap(self._s.moved(_translation(v)))

    def rotate(self, base, axis, degrees):
        self._set(self._s.moved(_rotation(base, axis, degrees)))
        return self

    def rotated(self, base, axis, degrees):
        return _wrap(self._s.moved(_rotation(base, axis, degrees)))

    def mirror(self, base, normal):
        return _wrap(self._s.mirrored(_v(base), _v(normal)))

    def _transform(self, m):
        m34 = m._m34()
        if m.hasScale() != 0:
            return self._s.gtransformed(m34)
        if m.determinant() < 0:
            return self._s.transformed(m34)
        return self._s.moved(m34)

    def transformShape(self, m, copy=False, checkScale=False):
        self._set(self._transform(m))
        return self

    def transformed(self, m, copy=False, checkScale=False, op=None):
        return _wrap(self._transform(m))

    def transformGeometry(self, m):
        return _wrap(self._s.gtransformed(m._m34()))

    def scale(self, factor, base=Vector()):
        self._set(self._scaled(factor, base))
        return self

    def scaled(self, factor, base=Vector()):
        return _wrap(self._scaled(factor, base))

    def _scaled(self, f, base):
        b = Vector(base)
        return self._s.gtransformed([f, 0, 0, b.x * (1 - f), 0, f, 0, b.y * (1 - f), 0, 0, f, b.z * (1 - f)])

    def copy(self, copyGeom=True, copyMesh=False):
        """A copy. Shapes are never modified in place by the kernel, so the geometry is shared."""
        return _wrap(self._s.moved([1.0, 0, 0, 0, 0, 1.0, 0, 0, 0, 0, 1.0, 0]))

    # -- queries
    def isInside(self, point, tolerance, checkFace):
        st = self._s.classify([_v(point)], float(tolerance))[0]
        return st == 0 or (st == 2 and bool(checkFace))

    def distToShape(self, other):
        d, sols = self._s.distance(_core(other))
        pairs = [(Vector(p), Vector(q)) for p, q in sols]
        return d, pairs, [None] * len(pairs)

    def slice(self, direction, d):
        """Section with the plane through the points p where direction·p = d: a list of wires."""
        n = Vector(direction)
        k = n.Length
        if k < EPS:
            raise ValueError('slice: null direction')
        out = self._s.slice(_v(n), float(d) / k)      # the kernel's plane is n̂·p = d
        return [_wrap(w) for w in out.children()]

    def slices(self, direction, ds):
        return [self.slice(direction, d) for d in ds]

    def tessellate(self, tolerance):
        verts, tris, _ = self._s.triangulate(float(tolerance), 0.5, False)
        pts = [Vector(verts[i], verts[i + 1], verts[i + 2]) for i in range(0, len(verts), 3)]
        return pts, [tuple(tris[i:i + 3]) for i in range(0, len(tris), 3)]

    def triangulate(self, linear, angular, relative=False):
        """(vertices flat, triangles flat, face per triangle) for fast export; angular in radians."""
        return self._s.triangulate(float(linear), float(angular), relative)

    def exportStl(self, path, linear=0.01, angular=0.5):
        self._s.write_stl(str(path), float(linear), float(angular))

    def check(self, runBopCheck=False):
        if not self.isValid():
            raise ValueError('shape is invalid')

    def fix(self, *args):
        self._set(self._s.fix())
        return True

    def __eq__(self, o):
        return isinstance(o, Shape) and self._s.is_same(o._s)

    __hash__ = object.__hash__


class Compound(Shape):
    def __init__(self, shapes=()):
        self._s = C.compound(_cores(shapes))
        self._cache = {}


class CompSolid(Shape):
    pass


class Solid(Shape):
    def __init__(self, shape):
        solids = _core(shape).subshapes(0)
        if not solids:
            raise OCCError('Part.Solid: the shape has no solid')
        self._s = solids[0]
        self._cache = {}


class Shell(Shape):
    pass


class Face(Shape):
    """Part.Face(wire) or Part.Face([outer, hole, …]): a face bounded by closed wires."""

    def __init__(self, *args):
        if len(args) == 1 and isinstance(args[0], Face):
            self._s, self._cache = args[0]._s, {}
            return
        wires = args[0] if len(args) == 1 and isinstance(args[0], (list, tuple)) else args
        if not wires:
            raise TypeError('Part.Face: give a closed wire')
        outer = wires[0]
        if not isinstance(outer, Shape):
            raise TypeError('Part.Face: give wires')
        self._s = C.face(outer._s, [w._s for w in wires[1:]])
        self._cache = {}

    @property
    def Surface(self):
        return self._cached('surface', lambda: _surface(self))

    def isPartOfDomain(self, u, v):
        return self._s.has_uv(float(u), float(v))

    def normalAt(self, u, v):
        raise NotImplementedError('Face.normalAt')

    @property
    def OuterWire(self):
        return _wrap(self._s.outer_wire())


class Wire(Shape):
    """Part.Wire(edges or wires)."""

    def __init__(self, parts):
        if isinstance(parts, Shape):
            parts = [parts]
        self._s = C.wire(_cores(parts))
        self._cache = {}

    def isClosed(self):
        v = self.Vertexes
        e = self.Edges
        return len(e) > 0 and (len(v) == len(e))

    discretize = None    # set below

    @property
    def OrderedEdges(self):
        return [_wrap(e) for e in self._s.ordered(4)]

    @property
    def OrderedVertexes(self):
        return [_wrap(v) for v in self._s.ordered(5)]


class Edge(Shape):
    @property
    def Curve(self):
        return self._cached('curve', lambda: _curve(self))

    def _info(self):
        return self._cached('info', self._s.edge_info)

    def firstVertex(self):
        return Vertex(Vector(self._info()[2]))

    def lastVertex(self):
        return Vertex(Vector(self._info()[4]))

    def isClosed(self):
        i = self._info()
        return Vector(i[2]).isEqual(Vector(i[4]), 1e-7)

    discretize = None


class Vertex(Shape):
    """Part.Vertex(point) or Part.Vertex(x, y, z)."""

    def __init__(self, *args):
        p = args[0] if len(args) == 1 else args
        self._s = C.vertex(_v(p))
        self._cache = {}

    @property
    def Point(self):
        return Vector(self._cached('point', self._s.point))

    @property
    def X(self):
        return self.Point.x

    @property
    def Y(self):
        return self.Point.y

    @property
    def Z(self):
        return self.Point.z


def _discretize(self, *args, **kw):
    """discretize(n), discretize(Number=n), discretize(Deflection=d) or discretize(QuasiDeflection=d)."""
    if args:
        mode, value = 0, args[0]
    elif 'Number' in kw:
        mode, value = 0, kw['Number']
    elif 'Deflection' in kw:
        mode, value = 1, kw['Deflection']
    elif 'QuasiDeflection' in kw:
        mode, value = 2, kw['QuasiDeflection']
    elif 'Distance' in kw:
        n = max(2, int(math.ceil(self.Length / kw['Distance'])) + 1)
        mode, value = 0, n
    else:
        raise TypeError('discretize: give a number of points or Deflection=')
    return [Vector(p) for p in self._s.discretize(mode, float(value))]


Wire.discretize = _discretize
Edge.discretize = _discretize

_CLASSES = {0: Compound, 1: CompSolid, 2: Solid, 3: Shell, 4: Face, 5: Wire, 6: Edge, 7: Vertex}


# --------------------------------------------------------------------------------------- curves
class Curve:
    """Base of the curve types an edge's Curve can be."""


class Line(Curve):
    """An infinite line (what Edge.Curve gives for a straight edge)."""

    def __init__(self, a=None, b=None):
        self.Location = Vector(a) if a is not None else Vector()
        d = (Vector(b) - self.Location) if b is not None else Vector(0, 0, 1)
        self.Direction = Vector(d).normalize() if d.Length > EPS else Vector(0, 0, 1)

    def toShape(self, *args):
        raise NotImplementedError('an infinite line has no shape; use Part.LineSegment')


class LineSegment(Line):
    def __init__(self, a, b):
        super().__init__(a, b)
        self.StartPoint, self.EndPoint = Vector(a), Vector(b)

    def toShape(self):
        return _wrap(C.segment(_v(self.StartPoint), _v(self.EndPoint)))


class Circle(Curve):
    def __init__(self, center=None, normal=None, radius=1.0):
        self.Center = Vector(center) if center is not None else Vector()
        self.Axis = Vector(normal) if normal is not None else Vector(0, 0, 1)
        self.Radius = float(radius)

    @property
    def Location(self):
        return self.Center


class Arc(Curve):
    """Part.Arc(p1, p2, p3): the circular arc through three points."""

    def __init__(self, a, b, c):
        self.points = (Vector(a), Vector(b), Vector(c))

    def toShape(self):
        return _wrap(C.arc3(*(_v(p) for p in self.points)))


class Ellipse(Curve):
    pass


class BSplineCurve(Curve):
    pass


class OtherCurve(Curve):
    pass


def _curve(edge):
    kind, length, a, m, b, circ = edge._info()
    if kind == 0:
        return Line(Vector(a), Vector(b))
    if kind == 1 and circ is not None:
        return Circle(Vector(circ[0]), Vector(circ[1]), circ[2])
    return {2: Ellipse, 6: BSplineCurve}.get(kind, OtherCurve)()


# --------------------------------------------------------------------------------------- surfaces
class Surface:
    """Base of the surface types a face's Surface can be. parameter(p) gives (u, v) of the nearest point."""

    def __init__(self, face=None, info=None):
        self._face = face
        if info is not None:
            kind, normal, point, ax = info
            if ax is not None:
                loc, d, r = ax
                self.Position = self.Center = Vector(loc)
                self.Axis = Vector(d)
                self.Radius = r

    def parameter(self, p):
        if self._face is None:
            raise NotImplementedError('Surface.parameter needs the surface of a face')
        return self._face._s.surface_uv(_v(p))


class Plane(Surface):
    pass


class Cylinder(Surface):
    pass


class Cone(Surface):
    pass


class Sphere(Surface):
    pass


class Toroid(Surface):
    pass


class BSplineSurface(Surface):
    pass


class OtherSurface(Surface):
    pass


def _surface(face):
    c = face.BoundBox.Center
    info = face._s.face_info(_v(c))
    cls = {0: Plane, 1: Cylinder, 2: Cone, 3: Sphere, 4: Toroid, 6: BSplineSurface}.get(info[0], OtherSurface)
    return cls(face, info)


# --------------------------------------------------------------------------------------- Part functions
_O, _Z = (0.0, 0.0, 0.0), (0.0, 0.0, 1.0)


def makeBox(length, width, height, pnt=None, dir=None):
    return _wrap(C.make_box(_v(pnt) if pnt is not None else _O, _v(dir) if dir is not None else _Z,
                            float(length), float(width), float(height)))


def makeCylinder(radius, height, pnt=None, dir=None, angle=360.0):
    return _wrap(C.make_cylinder(_v(pnt) if pnt is not None else _O, _v(dir) if dir is not None else _Z,
                                 float(radius), float(height), float(angle)))


def makeCone(radius1, radius2, height, pnt=None, dir=None, angle=360.0):
    return _wrap(C.make_cone(_v(pnt) if pnt is not None else _O, _v(dir) if dir is not None else _Z,
                             float(radius1), float(radius2), float(height), float(angle)))


def makeSphere(radius, pnt=None, dir=None, angle1=-90.0, angle2=90.0, angle3=360.0):
    return _wrap(C.make_sphere(_v(pnt) if pnt is not None else _O, _v(dir) if dir is not None else _Z,
                               float(radius), float(angle1), float(angle2), float(angle3)))


def makePolygon(points, closed=False):
    return _wrap(C.polygon([_v(p) for p in points], bool(closed)))


def makeLine(a, b):
    return _wrap(C.segment(_v(a), _v(b)))


def makeCompound(shapes):
    return _wrap(C.compound(_cores(shapes)))


def makeLoft(sections, solid=False, ruled=False, closed=False, maxDegree=5):
    if closed:
        raise NotImplementedError('makeLoft: closed lofts are not supported')
    return _wrap(C.loft(_cores(sections), bool(solid), bool(ruled), False))


def show(shape, name=None):
    """No document here: does nothing."""


Part = types.ModuleType('Part', 'FreeCAD-style Part module on cad-rust-app (cadrust.freecad)')
for _n, _o in dict(Shape=Shape, Compound=Compound, CompSolid=CompSolid, Solid=Solid, Shell=Shell, Face=Face,
                   Wire=Wire, Edge=Edge, Vertex=Vertex, Curve=Curve, Line=Line, LineSegment=LineSegment,
                   Circle=Circle, Arc=Arc, Ellipse=Ellipse, BSplineCurve=BSplineCurve, Surface=Surface, Plane=Plane,
                   Cylinder=Cylinder, Cone=Cone, Sphere=Sphere, Toroid=Toroid, BSplineSurface=BSplineSurface,
                   makeBox=makeBox, makeCylinder=makeCylinder, makeCone=makeCone, makeSphere=makeSphere,
                   makePolygon=makePolygon, makeLine=makeLine, makeCompound=makeCompound, makeLoft=makeLoft,
                   show=show, OCCError=OCCError).items():
    setattr(Part, _n, _o)

App = types.ModuleType('FreeCAD', 'FreeCAD-style App module on cad-rust-app (cadrust.freecad): no document, no GUI')
for _n, _o in dict(Vector=Vector, Matrix=Matrix, Rotation=Rotation, Placement=Placement, BoundBox=BoundBox,
                   GuiUp=False, Base=types.SimpleNamespace(Vector=Vector, Matrix=Matrix, Rotation=Rotation,
                                                           Placement=Placement, BoundBox=BoundBox)).items():
    setattr(App, _n, _o)
