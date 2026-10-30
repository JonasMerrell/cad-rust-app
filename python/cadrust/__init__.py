"""Python scripting for rCAD.

`cadrust._core` is the OpenCascade layer (shapes, operations, queries). `cadrust.freecad` puts a
FreeCAD-style API on it, so scripts written for FreeCAD's `Part` module run with an import change:

    from cadrust.freecad import App, Part, Vector, Matrix
"""

from ._core import OCCError, Shape  # noqa: F401
