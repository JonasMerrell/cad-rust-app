# Printing with OrcaSlicer

rCAD and the OrcaSlicer fork ([JonasMerrell/OrcaSlicer](https://github.com/JonasMerrell/OrcaSlicer))
work as a pair: parts go to the slicer as STEP, stay linked to the part they came from, and come
back to rCAD from the slicer's right-click menu.

## Sending parts

**File > Print in OrcaSlicer** (or the command search: "Print in OrcaSlicer") sends:

- in a Part Studio, the selected parts, or every part in sight when none is selected;
- in an assembly, the parts of the selected instances, or of every instance in sight (each part once).

Each part becomes its own STEP file, in the part's own coordinates, under the document's folder:
`.rcad-print/<Part Studio>/<part>.step` (in the temp folder while the document is unsaved). The
folder has a `.gitignore` of its own and is left out of project ZIPs, since rCAD makes it again
from the parts.

rCAD starts the slicer with `--single-instance`, so the files open in the slicer window that is
already up. The fork reloads a file it already has in place, keeping the object's placement,
orientation and settings, so sending a part again updates it instead of adding a copy. With
**Keep OrcaSlicer updated** on (File > 3D printing settings, on by default), rCAD sends the parts
the slicer has again by itself whenever they change.

Slicing, the G-code preview and sending to Klipper stay in OrcaSlicer, which already talks to
the printer.

## Back to rCAD

Next to each file rCAD writes `<file>.rcad.json`:

```json
{
  "format": "rcad-link-1",
  "app": "/path/to/rcad",
  "document": "/path/to/project/box.json",
  "studio": "Part Studio 1",
  "part": "Lid",
  "link": "127.0.0.1:41733"
}
```

For an object made from such a file, the fork's object and part menus have **Edit in rCAD**.
It asks the rCAD at `link` to show the part (`GET /edit?document=…&studio=…&part=…&file=…`;
rCAD listens on 127.0.0.1 only, from the first send on). When no rCAD answers, it starts `app`
as `rcad <document> --studio <studio> --part <part>`. rCAD opens the document if another one is
open (asking to save first when that has unsaved changes), switches to the Part Studio and
selects the part.

## Where the slicer is

File > 3D printing settings > OrcaSlicer takes the slicer program. Left empty, rCAD looks for the
fork's build (`~/OrcaSlicer/build/package/bin/orca-slicer`), then the usual install places
(`Program Files\OrcaSlicer\orca-slicer.exe`, `/Applications/OrcaSlicer.app`) and `PATH`.

Files from rCAD skip the fork's STEP mesh dialog and load with its default mesh settings. The
fork's command line also takes STEP files, so headless slicing doesn't need an STL step.
