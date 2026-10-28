# cadrust: Python scripting for cad-rust-app

Python bindings to cad-rust-app's OpenCascade kernel, its renderer and its finite elements, for
parametric models written as Python scripts (`python model.py`, no window).

    cd python && CMAKE_POLICY_VERSION_MINIMUM=3.5 maturin develop --release   # into the active environment

The first build compiles OpenCascade (about 20 minutes).

## Modules

- `cadrust.freecad`: a FreeCAD-style API with FreeCAD's semantics, so scripts written for FreeCAD's `Part`
  module run with an import change:

  ```python
  from cadrust.freecad import App, Part, Vector, Matrix

  box = Part.makeBox(10, 20, 30)
  part = box.cut(Part.makeCylinder(3, 40, Vector(5, 10, -5))).removeSplitter()
  part.Volume, part.BoundBox.ZLength, len(part.Faces)
  ```

  It covers:
  - `Vector`, `Matrix`, `Rotation`, `Placement` and `BoundBox`;
  - `makeBox`, `makeCylinder`, `makeCone`, `makeSphere`, `makePolygon`, `makeLoft` and `makeCompound`;
  - `Face`, `Wire`, `Vertex`, `LineSegment` and `Arc`;
  - booleans with one or many tools, `removeSplitter`, `makeFillet`, `makeChamfer`, `extrude` and
    `makeOffset2D`;
  - `translate`, `rotate`, `mirror` and `transformShape`;
  - `isInside`, `distToShape` and `slice`;
  - `Edge.Curve` (`Line`, `Circle`) and `Face.Surface` (`Plane`, `Cylinder`, …), `discretize`,
    `OuterWire` and `OrderedVertexes`;
  - `exportStl`.

  There is no document and no GUI (`App.GuiUp` is False).
- `cadrust.view`: images without a window. `Item(shape, color, opacity, placement, edges)`, a `Camera`
  (standard views, or any direction, fitted to the items or to a box) and `render(items, camera, w, h)`,
  which returns a PIL image. `camera.project(point)` gives pixel positions for labels.
- `cadrust.web`: interactive pages for a web browser (one .html file; three.js comes from a CDN). Parts sit on
  a tree of nodes that slide or turn within limits, moved by sliders, by dragging the parts, by the arrow
  keys and by scripted tours; layers can be shown, ghosted or hidden; a section plane cuts the model; markers
  read out a point in another node's frame and trace its path; parts whose shape follows the joints (cable
  chains, bellows) are sampled and blended.

  ```python
  from cadrust.web import Scene
  sc = Scene('Slide')
  base = sc.node('Base')
  car = sc.node('Carriage', parent=base, slider=(1, 0, 0), limits=(-50, 50), label='X')
  screw = sc.node('Screw', parent=base, revolute=(1, 0, 0), origin=(0, 0, 20), follows=car, ratio=90)  # 4 mm lead
  sc.part(base_shape, base, color=(0.5, 0.5, 0.52), layer='Castings')
  sc.part(carriage_shape, car, info={'Mass': '1.2 kg'})
  sc.marker('Tool', car, (0, 0, 40), relative=base, trail=True)
  sc.tour('Back and forth', [({car: -50}, 20), ({car: 50}, 20)])    # (joint values, feed mm/s)
  sc.write('slide.html')
  ```

  The app's Export menu (and its `export_web` tool) writes the same page from an assembly and its mates.
- `cadrust._core`: the layer underneath. It holds:
  - `Shape` and its operations and queries;
  - `write_step` (an assembly with names and colors);
  - `common_volumes` (interference volumes of many pairs, threaded);
  - `web_mesh` and `web_write` (under `cadrust.web`);
  - `render_meshes`;
  - `FeModel`, finite elements:

  ```python
  from cadrust import _core as C
  fm = C.FeModel()
  fm.add_body(casting._s, 30e3, 0.25, 2.35, 12.0)    # E (MPa), nu, density (g/cm3), element size (mm)
  fm.add_body(tube._s, 200e3, 0.25, 7.85, 2.5)       # a thin wall meshed finer
  fm.mesh()                                          # each part at its size, tied where they touch
  base = fm.patch((x0, y0, z0, x1, y1, z1), None, (0, 0, -1))
  top = fm.patch(...)
  fm.fix(base)                                       # or fix(patch, "z"), spring(a, b or None, (kx, ky, kz))
  fm.mass(top, 22.2)                                 # kg, spread over the patch
  fm.solve([[(top, (1000, 0, 0))], [(top, (0, 1000, 0))]])   # load cases on one factorization
  fm.displacement(0, top)                            # average over the patch, mm
  fm.modal(6), fm.energy_shares(0)
  ```
