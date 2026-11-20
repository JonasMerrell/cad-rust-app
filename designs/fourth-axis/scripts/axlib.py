"""Builder for rCAD `run` scripts: one document, one Part Studio per part, every part drawn in the
assembly's own coordinates (so the assembly needs no mates), prisms along x, y or z.

Frame used by the 4th axis: X along the rotation axis (+X from the faceplate toward the tailstock),
Y toward the back of the machine, Z up; origin on the rotation axis at the faceplate's front face.
"""
import json, math, os, subprocess

RCAD = os.path.expanduser("~/dmc2/rcad/target/release/rcad")


def C(u, v, r): return {"type": "circle", "center": [round(u, 4), round(v, 4)], "radius": round(r, 4)}
def R(u0, v0, u1, v1): return {"type": "rect", "corner1": [round(u0, 4), round(v0, 4)], "corner2": [round(u1, 4), round(v1, 4)]}
def poly(pts): return {"type": "polyline", "points": [[round(x, 4), round(y, 4)] for x, y in pts], "closed": True}


def rrect(u0, v0, u1, v1, r):
    """Rounded rectangle as lines + arcs (a list of items): inside corners a 1/4" end mill can cut need r >= 3.5."""
    u0, u1 = min(u0, u1), max(u0, u1); v0, v1 = min(v0, v1), max(v0, v1)
    L = lambda a, b: {"type": "line", "from": [round(a[0], 4), round(a[1], 4)], "to": [round(b[0], 4), round(b[1], 4)]}
    A = lambda c, s, e: {"type": "arc", "center": [round(c[0], 4), round(c[1], 4)], "start": [round(s[0], 4), round(s[1], 4)],
                         "end": [round(e[0], 4), round(e[1], 4)]}
    return [L((u0 + r, v0), (u1 - r, v0)), A((u1 - r, v0 + r), (u1 - r, v0), (u1, v0 + r)),
            L((u1, v0 + r), (u1, v1 - r)), A((u1 - r, v1 - r), (u1, v1 - r), (u1 - r, v1)),
            L((u1 - r, v1), (u0 + r, v1)), A((u0 + r, v1 - r), (u0 + r, v1), (u0, v1 - r)),
            L((u0, v1 - r), (u0, v0 + r)), A((u0 + r, v0 + r), (u0, v0 + r), (u0 + r, v0))]


def slot(u0, v0, u1, v1, r):
    """Obround between two centres."""
    d = math.hypot(u1 - u0, v1 - v0); nx, ny = -(v1 - v0)/d*r, (u1 - u0)/d*r
    L = lambda a, b: {"type": "line", "from": [round(a[0], 4), round(a[1], 4)], "to": [round(b[0], 4), round(b[1], 4)]}
    A = lambda c, s, e: {"type": "arc", "center": [round(c[0], 4), round(c[1], 4)], "start": [round(s[0], 4), round(s[1], 4)],
                         "end": [round(e[0], 4), round(e[1], 4)]}
    return [L((u0 - nx, v0 - ny), (u1 - nx, v1 - ny)), A((u1, v1), (u1 - nx, v1 - ny), (u1 + nx, v1 + ny)),
            L((u1 + nx, v1 + ny), (u0 + nx, v0 + ny)), A((u0, v0), (u0 + nx, v0 + ny), (u0 - nx, v0 - ny))]


def bolt_circle(n, pcd, r, phase=0.0, cu=0.0, cv=0.0):
    return [C(cu + pcd/2*math.cos(math.radians(phase + 360*i/n)), cv + pcd/2*math.sin(math.radians(phase + 360*i/n)), r)
            for i in range(n)]


def flat(items):
    out = []
    for i in items:
        if isinstance(i, list): out += flat(i)
        else: out.append(i)
    return out


class Part:
    def __init__(self, doc, name, material, color, kind="machined", stock="", note=""):
        self.doc, self.name, self.material, self.color = doc, name, material, color
        self.kind, self.stock, self.note = kind, stock, note
        self.fid = 0; self.body = None; self.studio = None

    def _s(self, tool, args): self.doc.steps.append({"tool": tool, "args": args})
    def _next(self): self.fid += 1; return self.fid

    def prism(self, axis, a0, a1, *items, op=None):
        """Material (or a cut) between a0 and a1 along `axis`; sketch coordinates are (y,z) for x, (x,z) for y, (x,y) for z."""
        a0, a1 = min(a0, a1), max(a0, a1)
        base, off = {"x": ("Right", a0), "y": ("Front", -a1), "z": ("Top", a0)}[axis]
        pl = self._next()
        self._s("add_feature", {"kind": "plane", "params": {"mode": "offset", "base": {"kind": "origin_plane", "plane": base},
                                                           "offset": str(round(off, 4))}})
        sk = self._next()
        self._s("create_sketch", {"plane": {"kind": "feature_plane", "feature": pl}})
        self._s("sketch_add", {"sketch_id": sk, "items": flat(list(items))})
        if op is None: op = "new" if self.body is None else "add"
        f = self._next()
        self._s("add_feature", {"kind": "extrude", "params": {"profiles": [{"sketch": sk}], "depth": str(round(a1 - a0, 4)), "op": op}})
        if self.body is None: self.body = [f, 0]
        return self

    def add(self, axis, a0, a1, *items): return self.prism(axis, a0, a1, *items)
    def cut(self, axis, a0, a1, *items): return self.prism(axis, a0, a1, *items, op="remove")

    def done(self):
        self._s("set_part", {"body": self.body, "name": self.name, "material": self.material, "color": self.color})
        return self


class Doc:
    """Collects steps; parts each in their own Part Studio; then an assembly with everything fixed in place."""
    def __init__(self):
        self.steps = [{"tool": "new_document", "args": {"units": "mm"}}]
        self.parts = []; self.n_studios = 0; self.inst = []; self.asm_steps = []; self.next_inst = 1
        self.hardware = []          # BOM rows for bought parts and screws

    def part(self, name, material="Aluminium 6061", color="#b9c0c8", **kw):
        if self.n_studios > 0 or self.parts:
            self.steps.append({"tool": "new_part_studio", "args": {"name": name}})
        p = Part(self, name, material, color, **kw); p.studio = self.n_studios; self.n_studios += 1
        self.parts.append(p); return p

    def vendor(self, name, path, color=None, material="Steel"):
        """Import a vendor STEP into its own studio; returns the studio index (bodies are listed after the run)."""
        if self.n_studios > 0: self.steps.append({"tool": "new_part_studio", "args": {"name": name}})
        self.steps.append({"tool": "import_file", "args": {"path": path, "name": name}})
        s = self.n_studios; self.n_studios += 1
        return s

    # ---- assembly (emitted after all studios) ----
    def place(self, part, name=None, position=(0, 0, 0), rotate=None, group=None):
        a = {"assembly": 0, "studio": part.studio, "body": part.body, "fixed": True, "name": name or part.name,
             "position": list(position)}
        if rotate: a["rotate"] = rotate
        self.asm_steps.append({"tool": "assembly_insert", "args": a})
        i = self.next_inst; self.next_inst += 1; self.inst.append((i, name or part.name, group)); return i

    def place_body(self, studio, body, name, position=(0, 0, 0), rotate=None, group=None):
        a = {"assembly": 0, "studio": studio, "body": body, "fixed": True, "name": name, "position": list(position)}
        if rotate: a["rotate"] = rotate
        self.asm_steps.append({"tool": "assembly_insert", "args": a})
        i = self.next_inst; self.next_inst += 1; self.inst.append((i, name, group)); return i

    def screw(self, size, length, seat, direction, std="ISO 4762", name=None, group=None, bom=None):
        """A screw whose seating plane (under the head) is at `seat`, shank pointing along `direction`."""
        a = {"assembly": 0, "standard": std, "size": size, "length": length, "position": [round(c, 4) for c in seat], "fixed": True}
        rot = rot_from_minus_z(direction)
        if rot: a["rotate"] = rot
        if name: a["name"] = name
        self.asm_steps.append({"tool": "insert_standard_part", "args": a})
        i = self.next_inst; self.next_inst += 1; self.inst.append((i, name or f"{std} {size}x{length}", group))
        return i

    def std(self, standard, size, base, direction=(0, 0, 1), length=None, name=None, group=None):
        """Nut, washer, pin, bearing: stands on z = 0 along +Z in its own frame; `direction` is where its +Z goes."""
        a = {"assembly": 0, "standard": standard, "size": size, "position": [round(c, 4) for c in base], "fixed": True}
        if length: a["length"] = length
        rot = rot_from_plus_z(direction)
        if rot: a["rotate"] = rot
        if name: a["name"] = name
        self.asm_steps.append({"tool": "insert_standard_part", "args": a})
        i = self.next_inst; self.next_inst += 1; self.inst.append((i, name or f"{standard} {size}", group)); return i

    def script(self, extra=()):
        return self.steps + [{"tool": "create_assembly", "args": {"name": "A axis"}}] + self.asm_steps + list(extra)


def rot_from_plus_z(d):
    n = math.sqrt(sum(c*c for c in d)); d = [c/n for c in d]
    if d[2] > 1 - 1e-9: return None
    if d[2] < -1 + 1e-9: return {"axis": [1, 0, 0], "angle": 180}
    ax = [-d[1], d[0], 0.0]                      # z × d
    return {"axis": ax, "angle": round(math.degrees(math.acos(d[2])), 6)}


def rot_from_minus_z(d):
    n = math.sqrt(sum(c*c for c in d)); d = [c/n for c in d]
    if d[2] < -1 + 1e-9: return None
    if d[2] > 1 - 1e-9: return {"axis": [1, 0, 0], "angle": 180}
    ax = [d[1], -d[0], 0.0]                      # (−z) × d
    return {"axis": ax, "angle": round(math.degrees(math.acos(-d[2])), 6)}


def run(steps, workdir, stem, timeout=7200):
    """Write <stem>.json in workdir, run rCAD there, return the list of result lines (dicts)."""
    os.makedirs(workdir, exist_ok=True)
    js = os.path.join(workdir, stem + ".json")
    json.dump(steps, open(js, "w"))
    with open(os.path.join(workdir, stem + ".out"), "w") as o, open(os.path.join(workdir, stem + ".err"), "w") as e:
        subprocess.run([RCAD, "run", js], cwd=workdir, stdout=o, stderr=e, timeout=timeout)
    out = []
    for line in open(os.path.join(workdir, stem + ".out")):
        try: out.append(json.loads(line))
        except Exception: pass
    return out


def failures(results):
    bad = []
    for r in results:
        if "error" in r: bad.append((r.get("step"), r.get("tool"), str(r["error"])[:300]))
        else:
            res = r.get("result", {})
            if isinstance(res, dict) and (res.get("ok") is False or res.get("errors")):
                bad.append((r.get("step"), r.get("tool"), str(res.get("errors") or res.get("new_errors"))[:300]))
    return bad
