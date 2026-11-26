"""Tests for the phone app's project routes (python3 -m unittest capture/receiver/test_projects.py).

A stub `rcad` stands in for the real program: `run` writes the files a script asks for, `chat`
prints the JSON lines `rcad chat` prints and saves a chat like src/claude_chats.rs does.
"""
import base64
import http.client
import json
import os
import stat
import sys
import tempfile
import textwrap
import threading
import time
import unittest
from http.server import ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rcad_capture_receiver as r  # noqa: E402
import rcad_projects as rp  # noqa: E402

STUB = textwrap.dedent('''\
    #!/usr/bin/env python3
    import json, os, sys, time
    a = sys.argv[1:]
    if a[0] == "run":
        steps = json.load(open(a[-1]))
        doc = None
        for s in steps:
            t, args = s["tool"], s["args"]
            if t == "open_document":
                doc = json.load(open(args["path"]))
            elif t == "new_document":
                doc = {"format": "cad-rust-app/workspace-1", "studios": [{"name": "Part Studio 1", "doc": {}}], "assemblies": []}
            elif t == "save_document":
                json.dump(doc, open(args["path"], "w"), indent=2)
            elif t == "export_web":
                if "assembly" in args and os.environ.get("STUB_SLOW_EXPORT"):
                    sys.stderr.write("Message_Alert: some kernel chatter\\n")
                    sys.stderr.write("Building Part Studio \u201cBody\u201d (1 of 2)\\n"); sys.stderr.flush()
                    time.sleep(float(os.environ["STUB_SLOW_EXPORT"]))
                open(args["path"], "w").write("<html>%s %s</html>" % (doc["studios"][0]["name"], json.dumps(args)))
            elif t == "describe":
                print(json.dumps({"tool": t, "result": {"units": "mm", "parts": [{"name": "Part 1", "volume_mm3": 12.0}],
                    "errors": [{"feature": 2, "name": "Fillet 1", "error": "radius too big"}],
                    "features": [{"id": 1, "name": "Sketch 1", "kind": "sketch", "suppressed": False, "params": {}},
                                 {"id": 2, "name": "Fillet 1", "kind": "fillet", "suppressed": False, "params": {"radius": "3", "edges": []}}],
                    "variables": [{"name": "w", "expr": "40"}]}}))
                continue
            elif t == "assembly_info":
                print(json.dumps({"tool": t, "result": {"name": "Arm", "instances": [{"id": 1, "name": "Body <1>", "fixed": True}],
                    "mates": [{"kind": "revolute", "name": "Hinge"}], "bom": [], "failed_mates": []}}))
                continue
            elif t in ("export_cam_simulation", "export_setup_sheet"):
                open(args["path"], "w").write("<html>%s %s</html>" % (t, json.dumps(args)))
            elif t == "solve_simulation":
                sys.stderr.write("Beam: Solving 50% (1 s)\\n"); sys.stderr.flush()
                time.sleep(1.5)
                print(json.dumps({"tool": t, "result": {"analysis": "static", "material": "Steel", "max_von_mises_mpa": 62.07,
                    "max_stress_at": [0, 0, 10], "factor_of_safety": 1.2, "max_displacement_mm": 0.2007, "warnings": [],
                    "reactions": [{"support": "Fixed 1", "magnitude_n": 100}], "material_props": {"E_mpa": 200000, "yield_mpa": 250},
                    "mesh": {"elements": 9024, "nodes": 15041, "element": "10-node tetrahedron", "element_size_mm": 2.5}}}))
                continue
            elif t == "export_simulation_web":
                open(args["path"], "w").write("<html>3D result</html>")
            elif t == "render_simulation":
                png = b"\\x89PNG\\r\\n\\x1a\\n" + b"\\x00\\x00\\x00\\rIHDR" + (900).to_bytes(4, "big") + (700).to_bytes(4, "big")
                open(args["path"], "wb").write(png)
            elif t == "render_pcb":
                png = b"\\x89PNG\\r\\n\\x1a\\n" + b"\\x00\\x00\\x00\\rIHDR" + (1400).to_bytes(4, "big") + (1000).to_bytes(4, "big")
                open(args["path"], "wb").write(png)
            elif t == "pcb_info":
                print(json.dumps({"tool": t, "result": {"name": "Blinker", "unrouted": 1, "board": {"size": [40, 30], "layers": 2, "tracks": 97},
                    "airwires": [{"net": "OUT", "from": [1, 2], "to": [3, 4.5]}],
                    "parts": [{"reference": "U1", "value": "NE555", "footprint": "SOIC-8", "bom": {"mpn": "C46749"}, "side": "top", "fitted": True}],
                    "nets": [{"name": "OUT", "pins": ["U1.3", "R3.1"]}], "lengths": [{"net": "OUT", "length": 19.06}]}}))
                continue
            elif t == "simulate_pcb":
                open(args["csv"], "w").write("time_s,V(OUT) V\\n0,0\\n0.1,3.2\\n0.2,0\\n")
                print(json.dumps({"tool": t, "result": {"analysis": "tran", "probes": {"V(OUT)": {"average": 1.945, "min": 0, "max": 3.267,
                    "frequency": 6.759, "duty": 0.5153, "unit": "V"}}, "notes": []}}))
                continue
            elif t == "export_drawing":
                open(args["path"], "w").write('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 297"><text>sheet %d</text></svg>' % args["sheet"])
            elif t == "note":
                d = doc["studios"][args["studio"]]["doc"]
                notes = d.setdefault("notes", [])
                if args["action"] == "add":
                    nid = max([n["id"] for n in notes] or [0]) + 1
                    notes.append({"id": nid, "text": args["text"], "at": args["at"], "normal": args["normal"]})
                    res = {"added": nid}
                elif not any(n["id"] == args["id"] for n in notes):
                    print(json.dumps({"tool": t, "error": "no note %d" % args["id"]}))
                    continue
                elif args["action"] == "delete" or not args["text"]:
                    d["notes"] = notes = [n for n in notes if n["id"] != args["id"]]
                    res = {"deleted": args["id"]}
                else:
                    for n in notes:
                        if n["id"] == args["id"]:
                            n["text"] = args["text"]
                    res = {"edited": args["id"]}
                res["notes"] = [{"id": n["id"], "text": n["text"], "at": n["at"]} for n in notes]
                print(json.dumps({"tool": t, "result": res}))
                continue
            elif t == "drawing_info":
                print(json.dumps({"tool": t, "result": {"name": "Plate", "sheets": 2, "sheet_size": "A3", "warnings": [],
                    "views": [{"id": 1, "dir": "front", "scale": 2, "sheet": 1}, {"id": 2, "dir": "section:front", "letter": "A", "scale": 2, "sheet": 1},
                              {"id": 3, "dir": "iso", "assembly": 0, "scale": 1, "sheet": 2}],
                    "dimensions": [{"view": 1, "kind": "horizontal", "text": "60"}, {"view": 3, "kind": "diameter", "text": "8"}],
                    "notes": [{"text": "Break sharp edges", "sheet": 1}], "gdt": [],
                    "title_block": {"part": "Plate", "material": "AlMg3", "first_angle": False}}}))
                continue
            elif t == "cam_info":
                print(json.dumps({"tool": t, "result": {"name": "Plate", "cycle_time_s": 970, "post": "mach3", "errors": [],
                    "tools": [{"id": 1, "number": 3, "description": "6 mm flat end mill", "rpm": 12000, "feed": 800}],
                    "operations": [{"name": "Pocket 1", "kind": "pocket", "tool": 1, "tool_number": 3, "time_s": 59, "warnings": []},
                                   {"name": "Contour 1", "kind": "contour", "tool": 1, "tool_number": 3, "time_s": 302,
                                    "warnings": ["cuts 13.5 mm deep but the flutes are only 13 mm long"]}],
                    "setup_notes": ["clamp it in the vise"]}}))
                continue
            elif t == "simulate_cam":
                print(json.dumps({"tool": t, "result": {"ok": False, "removed_mm3": 5000, "problems": [
                    {"operation": "Pocket 1", "what": "tool overloaded", "at": [1, 2, -7]},
                    {"operation": "Pocket 1", "what": "tool overloaded", "at": [3, 2, -7]}]}}))
                continue
            print(json.dumps({"tool": t, "result": {"ok": True}}))
        sys.exit(0)
    if a[0] == "github":
        o = dict(zip(a[2::2], a[3::2]))
        log = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "github.log")
        open(log, "a").write(json.dumps(a[1:]) + "\\n")
        if a[1] == "status":
            print(json.dumps({"signed_in": True, "login": "octo", "name": "Octo Cat", "source": "gh"}))
        elif a[1] == "login-start":
            print(json.dumps({"user_code": "ABCD-1234", "verification_uri": "https://github.com/login/device",
                              "device_code": "dev-secret", "interval": 5, "expires_in": 900}))
        elif a[1] == "login-poll":
            assert o["--code"] == "dev-secret"
            print(json.dumps({"state": "done", "login": "octo", "name": "Octo Cat"}))
        elif a[1] == "repos":
            print(json.dumps({"repos": [
                {"full_name": "octo/bracket", "name": "bracket", "private": False, "description": "Made with rCAD",
                 "clone_url": "https://github.com/octo/bracket.git", "pushed": "2026-11-20T10:00:00Z", "rcad": True},
                {"full_name": "octo/gearbox", "name": "gearbox", "private": True, "description": "",
                 "clone_url": "https://github.com/octo/gearbox.git", "pushed": "2026-11-19T10:00:00Z", "rcad": False},
                {"full_name": "octo/empty", "name": "empty", "private": True, "description": "",
                 "clone_url": "https://github.com/octo/empty.git", "pushed": "", "rcad": False}]}))
        elif a[1] == "clone":
            name = o["--url"].rstrip("/").rsplit("/", 1)[1].removesuffix(".git")
            d = os.path.join(o["--into"], name)
            os.makedirs(os.path.join(d, ".git"), exist_ok=True)
            open(os.path.join(d, ".git", "config"), "w").write('[core]\\n\\tbare = false\\n[remote "origin"]\\n\\turl = %s\\n' % o["--url"])
            doc = os.path.join(d, name + ".json")
            if name != "empty":
                json.dump({"format": "cad-rust-app/workspace-1", "studios": [{"name": "Gears", "doc": {}}]}, open(doc, "w"))
            print(json.dumps({"doc": doc, "empty": name == "empty"}))
        elif a[1] in ("sync", "resolve"):
            print(json.dumps({"committed": "Save", "synced": "Pushed 1 commit to octo/" + os.path.basename(os.path.dirname(o["--doc"]))}))
        elif a[1] == "logout":
            print(json.dumps({"ok": True, "signed_in": False}))
        sys.exit(0)
    if a[0] == "chat":
        imgs = [a[i + 1] for i, x in enumerate(a) if x == "--image"]
        json.dump({"images": imgs, "sizes": [os.path.getsize(p) for p in imgs]},
                  open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "chat-args.json"), "w"))
        a = [x for i, x in enumerate(a) if x != "--image" and (i == 0 or a[i - 1] != "--image")]
        o = dict(zip(a[1::2], a[2::2]))
        doc, prompt = o["--doc"], open(o["--prompt-file"]).read()
        cid = o.get("--chat", "c1")
        print(json.dumps({"type": "chat", "id": cid}), flush=True)
        print(json.dumps({"type": "text", "text": "On it: " + prompt}), flush=True)
        if prompt == "slow":
            for line in sys.stdin:
                if line.strip() == "stop":
                    break
            print(json.dumps({"type": "done", "error": "Stopped"}), flush=True)
            sys.exit(1)
        ws = json.load(open(doc))
        ws["studios"][0]["name"] = prompt
        json.dump(ws, open(doc, "w"))
        d = os.path.join(os.path.dirname(doc), ".rcad-chats")
        os.makedirs(d, exist_ok=True)
        json.dump({"format": 1, "id": cid, "document": os.path.basename(doc), "title": prompt, "created": 1, "updated": 2,
                   "model": o.get("--model", ""), "lines": [{"kind": "user", "text": prompt}, {"kind": "assistant", "text": "done"}]},
                  open(os.path.join(d, cid + ".json"), "w"))
        print(json.dumps({"type": "saved", "path": doc}), flush=True)
        print(json.dumps({"type": "done", "error": None}), flush=True)
''')


BODY = {
    "format": "cad-rust-app/1", "units": "mm", "rollback": 2,
    "variables": [{"name": "H", "expr": "15"}],
    "features": [
        {"id": 1, "name": "Sketch 1", "suppressed": False, "hidden": False,
         "op": {"type": "sketch", "plane": {"kind": "origin_plane", "plane": "Top"},
                "sketch": {"curves": [1, 2, 3, 4], "constraints": [1], "texts": []}}},
        {"id": 2, "name": "Block", "op": {"type": "extrude", "depth": "H", "end": "blind", "op": "add"}},
        {"id": 3, "name": "Pocket", "suppressed": True, "op": {"type": "extrude", "depth": "4", "end": "blind", "op": "remove"}},
        {"id": 4, "name": "Round", "op": {"type": "fillet", "radius": "3", "edges": [1, 2, 3, 4]}},
    ],
}


class ProjectsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.projects_dir = os.path.join(self.tmp, "rCAD Projects")
        os.makedirs(os.path.join(self.projects_dir, "Bracket", ".rcad-cache"))
        with open(os.path.join(self.projects_dir, "Bracket", "bracket.json"), "w") as f:
            json.dump({"format": "cad-rust-app/workspace-1", "studios": [{"name": "Body", "doc": BODY}],
                       "assemblies": [{"name": "Arm"}], "cams": [{"name": "Plate"}],
                       "drawings": [{"name": "Plate"}],
                       "simulations": [{"name": "Beam", "analysis": "static", "material": "Steel",
                                        "supports": [{"name": "Fixed 1", "kind": "fixed", "faces": [{}]}],
                                        "loads": [{"name": "Force 1", "kind": "force", "value": 100, "faces": [{}]}]}],
                       "pcbs": [{"name": "Blinker", "sim": {"probes": ["OUT"]}}]}, f, indent=2)
        with open(os.path.join(self.projects_dir, "Bracket", "notes.json"), "w") as f:
            f.write('{"not": "a document"}')
        stub = os.path.join(self.tmp, "rcad")
        with open(stub, "w") as f:
            f.write(STUB)
        os.chmod(stub, os.stat(stub).st_mode | stat.S_IEXEC)
        rp.CACHE_DIR = os.path.join(self.tmp, "cache")
        projects = rp.Projects([self.projects_dir], stub)
        r.Handler.app = r.App(os.path.join(self.tmp, "scans"), "tok", (), projects)
        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), r.Handler)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.port = self.srv.server_address[1]

    def tearDown(self):
        self.srv.shutdown()

    def req(self, method, path, body=None, token="tok", raw=False):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        h = {"Authorization": "Bearer " + token} if token else {}
        c.request(method, path, body=json.dumps(body).encode() if body is not None else None, headers=h)
        resp = c.getresponse()
        data = resp.read()
        c.close()
        return resp.status, (data.decode() if raw else json.loads(data or b"{}"))

    def doc(self):
        st, res = self.req("GET", "/api/projects")
        self.assertEqual(st, 200)
        return res["documents"]

    def wait_job(self, job):
        after, evs = 0, []
        for _ in range(40):
            st, res = self.req("GET", f"/api/jobs/{job}?after={after}&wait=5")
            self.assertEqual(st, 200)
            evs += res["events"]
            after = res["next"]
            if res["done"]:
                return evs
        self.fail("request never finished")

    def test_list_and_view(self):
        self.assertEqual(self.req("GET", "/api/projects", token="bad")[0], 401)
        docs = self.doc()
        self.assertEqual([d["name"] for d in docs], ["bracket"])
        did = docs[0]["id"]
        st, d = self.req("GET", f"/api/doc/{did}")
        self.assertEqual((st, d["studios"], d["assemblies"]), (200, ["Body"], ["Arm"]))
        tree = d["trees"][0]
        self.assertEqual([(f["name"], f["type"], f["detail"], f["suppressed"]) for f in tree["features"]], [
            ("Sketch 1", "sketch", "Top plane · 4 curves · 1 constraint", False),
            ("Block", "extrude", "depth H", False),
            ("Pocket", "extrude", "4 mm · cut", True),
            ("Round", "fillet", "3 mm · 4 edges", False),
        ])
        self.assertEqual((tree["rollback"], tree["variables"]), (2, [{"name": "H", "expr": "15"}]))
        # the web view takes the token in the query too
        st, page = self.req("GET", f"/api/doc/{did}/view?t=tok", token=None, raw=True)
        self.assertEqual(st, 200)
        self.assertIn("Body", page)
        st, page = self.req("GET", f"/api/doc/{did}/view?assembly=0", raw=True)
        self.assertIn('"assembly": 0', page)
        self.assertEqual(self.req("GET", f"/api/doc/{did}/view?t=bad", token=None)[0], 401)
        # the feature tree that goes with the page
        st, t = self.req("GET", f"/api/doc/{did}/tree")
        self.assertEqual((st, t["kind"]), (200, "studio"))
        self.assertEqual([f["name"] for f in t["features"]], ["Sketch 1", "Fillet 1"])
        self.assertEqual((t["features"][1]["detail"], t["features"][1]["error"]), ("radius 3", "radius too big"))
        self.assertEqual(t["variables"], [{"name": "w", "expr": "40"}])
        st, t = self.req("GET", f"/api/doc/{did}/tree?assembly=0")
        self.assertEqual((t["kind"], t["instances"][0]["name"], t["mates"][0]["name"]), ("assembly", "Body <1>", "Hinge"))

    def test_cam_tab(self):
        did = self.doc()[0]["id"]
        st, d = self.req("GET", f"/api/doc/{did}")
        self.assertEqual(d["cams"], ["Plate"])
        st, page = self.req("GET", f"/api/doc/{did}/view?cam=0", raw=True)
        self.assertEqual(st, 200)
        self.assertIn("export_cam_simulation", page)
        st, page = self.req("GET", f"/api/doc/{did}/view?cam=0&page=sheet", raw=True)
        self.assertIn("export_setup_sheet", page)
        st, t = self.req("GET", f"/api/doc/{did}/tree?cam=0")
        self.assertEqual((st, t["kind"]), (200, "cam"))
        self.assertEqual(t["summary"], "16 min 10 s cycle · 2 operations · 5.0 cm³ removed · mach3")
        self.assertEqual([p["label"] for p in t["pages"]], ["Simulation", "Setup sheet"])
        sec = {s["title"]: s for s in t["sections"]}
        self.assertEqual(list(sec), ["Problems", "Operations", "Tools", "Before running"])
        # the same problem in one operation is one row
        self.assertEqual([r["name"] for r in sec["Problems"]["rows"]], ["2× tool overloaded"])
        self.assertEqual(sec["Problems"]["rows"][0]["sub"], "Pocket 1 first at X1 Y2 Z-7")
        ops = sec["Operations"]["rows"]
        self.assertEqual((ops[0]["sub"], ops[0]["icon"], ops[1]["error"]), ("T3 · 6 mm flat end mill · 59 s", "camPocket", True))
        self.assertEqual(self.req("GET", f"/api/doc/{did}/view?cam=5")[0], 404)

    def test_drawing_tab(self):
        did = self.doc()[0]["id"]
        self.assertEqual(self.req("GET", f"/api/doc/{did}")[1]["drawings"], ["Plate"])
        st, page = self.req("GET", f"/api/doc/{did}/view?drawing=0", raw=True)
        self.assertEqual(st, 200)
        self.assertIn("<text>sheet 1</text>", page)
        self.assertIn("Plate · sheet 1", page)
        st, page = self.req("GET", f"/api/doc/{did}/view?drawing=0&page=2", raw=True)
        self.assertIn("<text>sheet 2</text>", page)
        st, t = self.req("GET", f"/api/doc/{did}/tree?drawing=0")
        self.assertEqual(t["summary"], "A3 · sheet 1 of 2 · 1 dimension · third-angle")
        self.assertEqual([(p["id"], p["label"]) for p in t["pages"]], [("", "Sheet 1"), ("2", "Sheet 2")])
        sec = {s["title"]: s for s in t["sections"]}
        self.assertEqual([r["name"] for r in sec["Views"]["rows"]], ["Front", "Section A"])
        self.assertEqual([r["name"] for r in sec["Dimensions"]["rows"]], ["60"])
        self.assertEqual(sec["Title block"]["rows"][1], {"name": "AlMg3", "sub": "Material", "icon": "properties",
                                                         "ask": "In the drawing's title block, set the material to "})
        st, t = self.req("GET", f"/api/doc/{did}/tree?drawing=0&page=2")
        self.assertEqual([r["name"] for r in {s["title"]: s for s in t["sections"]}["Views"]["rows"]], ["Iso"])

    def quick_wait(self, secs):
        old = rp.QUICK_WAIT
        rp.QUICK_WAIT = secs
        self.addCleanup(setattr, rp, "QUICK_WAIT", old)

    def test_big_assembly_builds_in_the_background(self):
        # a page that builds quickly comes straight back (test_list_and_view); a slow one shows progress
        self.quick_wait(0.5)
        os.environ["STUB_SLOW_EXPORT"] = "2"
        self.addCleanup(os.environ.pop, "STUB_SLOW_EXPORT", None)
        did = self.doc()[0]["id"]
        st, page = self.req("GET", f"/api/doc/{did}/view?assembly=0&app=1", raw=True)
        self.assertEqual(st, 200)
        self.assertIn("Building the assembly on netdash", page)
        self.assertIn("RcadApp.building()", page)
        time.sleep(0.5)
        st, t = self.req("GET", f"/api/doc/{did}/tree?assembly=0")
        self.assertEqual((t["kind"], t["building"]), ("assembly", True))
        self.assertEqual(t["progress"], "Building Part Studio \u201cBody\u201d (1 of 2)")  # not the kernel's chatter
        for _ in range(50):
            st, page = self.req("GET", f"/api/doc/{did}/view?assembly=0&app=1", raw=True)
            if "Building the assembly" not in page:
                break
            time.sleep(0.2)
        self.assertIn('"assembly": 0', page)
        st, t = self.req("GET", f"/api/doc/{did}/tree?assembly=0")
        self.assertEqual(t["instances"][0]["name"], "Body <1>")

    def test_pages_go_out_gzipped(self):
        import gzip as gz
        old = rp.GZIP_MIN
        rp.GZIP_MIN = 0
        self.addCleanup(setattr, rp, "GZIP_MIN", old)
        did = self.doc()[0]["id"]
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        c.request("GET", f"/api/doc/{did}/view?studio=0&t=tok", headers={"Accept-Encoding": "gzip, deflate"})
        r = c.getresponse()
        body = r.read()
        c.close()
        self.assertEqual(r.getheader("Content-Encoding"), "gzip")
        self.assertIn(b"<html>Body", gz.decompress(body))
        # a client that doesn't take gzip gets the page as it is
        st, page = self.req("GET", f"/api/doc/{did}/view?studio=0", raw=True)
        self.assertTrue(page.startswith("<html>Body"))

    def test_simulation_tab_solves_in_the_background(self):
        self.quick_wait(0)
        did = self.doc()[0]["id"]
        self.assertEqual(self.req("GET", f"/api/doc/{did}")[1]["simulations"], ["Beam"])
        # the first ask starts the solve and gets a page that reloads itself
        st, page = self.req("GET", f"/api/doc/{did}/view?simulation=0", raw=True)
        self.assertEqual(st, 200)
        self.assertIn("Solving the simulation", page)
        self.assertIn("location.reload()", page)
        st, t = self.req("GET", f"/api/doc/{did}/tree?simulation=0")
        self.assertEqual((t["kind"], t["building"]), ("simulation", True))
        for _ in range(50):
            st, page = self.req("GET", f"/api/doc/{did}/view?simulation=0", raw=True)
            if "Solving" not in page:
                break
            time.sleep(0.2)
        self.assertEqual(page, "<html>3D result</html>")
        # the pictures come from the same solve: no new build
        st, page = self.req("GET", f"/api/doc/{did}/view?simulation=0&page=pictures", raw=True)
        self.assertIn('"label": "Stress"', page)
        self.assertIn('"label": "Safety"', page)
        self.assertIn("data:image/png;base64,", page)
        st, t = self.req("GET", f"/api/doc/{did}/tree?simulation=0")
        self.assertEqual(t["summary"], "Static · Steel · max 62.1 MPa · safety 1.2 · 0.201 mm")
        self.assertEqual([p["label"] for p in t["pages"]], ["3D", "Pictures"])
        st, t2 = self.req("GET", f"/api/doc/{did}/tree?simulation=0&page=pictures")
        self.assertEqual(t2, t)
        sec = {s["title"]: s for s in t["sections"]}
        self.assertEqual(sec["Results"]["rows"][0]["name"], "Factor of safety 1.2")
        self.assertTrue(sec["Results"]["rows"][0]["error"])
        self.assertEqual([r["name"] for r in sec["Supports and loads"]["rows"]], ["Fixed 1", "Force 1"])
        self.assertEqual(sec["Supports and loads"]["rows"][1]["sub"], "force · 100 N")

    def test_pcb_tab(self):
        did = self.doc()[0]["id"]
        self.assertEqual(self.req("GET", f"/api/doc/{did}")[1]["pcbs"], ["Blinker"])
        st, page = self.req("GET", f"/api/doc/{did}/view?pcb=0", raw=True)
        self.assertEqual(st, 200)
        for label in ("Top", "Bottom", "Schematic", "Circuit"):
            self.assertIn(f'"label": "{label}"', page)
        self.assertIn("V(OUT) V", page)
        st, page = self.req("GET", f"/api/doc/{did}/view?pcb=0&page=3d", raw=True)
        self.assertIn("<html>", page)  # the stub's export_web page
        st, t = self.req("GET", f"/api/doc/{did}/tree?pcb=0")
        self.assertEqual(t["summary"], "40 × 30 mm · 2 layers · 1 parts · 97 tracks · 1 unrouted")
        sec = {s["title"]: s for s in t["sections"]}
        self.assertEqual(sec["Unrouted"]["rows"][0]["sub"], "from X1 Y2 to X3 Y4.5")
        self.assertEqual(sec["Circuit"]["rows"][0]["name"], "V(OUT) average 1.95 V")
        self.assertEqual(sec["Circuit"]["rows"][0]["sub"], "0 V to 3.27 V · 6.76 Hz · duty 52 %")
        self.assertEqual((sec["Parts"]["rows"][0]["name"], sec["Parts"]["rows"][0]["sub"]), ("U1 NE555", "SOIC-8 · C46749"))
        self.assertEqual(sec["Nets"]["rows"][0]["sub"], "U1.3, R3.1 · 19.1 mm")

    def test_ids_stay_inside_the_project_folders(self):
        outside = os.path.join(self.tmp, "secret.json")
        with open(outside, "w") as f:
            f.write('{"format": "cad-rust-app/workspace-1", "studios": []}')
        for rel in ["../secret.json", "/etc/passwd", "Bracket/../../secret.json"]:
            did = "0-" + base64.urlsafe_b64encode(rel.encode()).decode().rstrip("=")
            self.assertEqual(self.req("GET", f"/api/doc/{did}")[0], 404, rel)
        self.assertEqual(self.req("GET", "/api/doc/5-QQ")[0], 404)
        self.assertEqual(self.req("GET", "/api/doc/0-!!")[0], 404)

    def test_chat_flow(self):
        did = self.doc()[0]["id"]
        st, res = self.req("POST", f"/api/doc/{did}/chat", {"text": "Lid", "model": "sonnet"})
        self.assertEqual(st, 200)
        self.assertEqual(res["chat_id"], "c1")
        evs = self.wait_job(res["job"])
        self.assertEqual([e["type"] for e in evs], ["chat", "text", "saved", "done"])
        self.assertIsNone(evs[-1]["error"])
        st, d = self.req("GET", f"/api/doc/{did}")
        self.assertEqual(d["studios"], ["Lid"])
        self.assertEqual([c["id"] for c in d["chat_list"]], ["c1"])
        self.assertEqual(d["chat_list"][0]["model"], "claude-sonnet-5-5")
        st, c = self.req("GET", f"/api/doc/{did}/chats/c1")
        self.assertEqual((st, c["lines"][0]["text"]), (200, "Lid"))
        self.assertEqual(self.req("GET", f"/api/doc/{did}/chats/..%2Fx")[0], 404)
        # the viewer page follows the change
        st, page = self.req("GET", f"/api/doc/{did}/view", raw=True)
        self.assertIn("Lid", page)
        self.assertEqual(self.req("POST", f"/api/doc/{did}/chat", {"text": " "})[0], 400)

    def test_stop_and_one_request_at_a_time(self):
        did = self.doc()[0]["id"]
        st, res = self.req("POST", f"/api/doc/{did}/chat", {"text": "slow"})
        self.assertEqual(st, 200)
        st, busy = self.req("POST", f"/api/doc/{did}/chat", {"text": "again"})
        self.assertEqual((st, busy["job"]), (409, res["job"]))
        self.assertTrue(self.doc()[0]["busy"])
        self.assertEqual(self.req("POST", f"/api/jobs/{res['job']}/stop", {})[0], 200)
        evs = self.wait_job(res["job"])
        self.assertEqual(evs[-1], {"type": "done", "error": "Stopped"})
        time.sleep(0.2)
        self.assertFalse(self.doc()[0]["busy"])

    def test_github(self):
        st, res = self.req("GET", "/api/github")
        self.assertEqual((st, res["login"]), (200, "octo"))
        st, res = self.req("POST", "/api/github/login", {})
        self.assertEqual((st, res["user_code"]), (200, "ABCD-1234"))
        self.assertNotIn("device_code", res)  # stays on netdash
        st, res = self.req("POST", "/api/github/login/poll", {})
        self.assertEqual((st, res["state"]), (200, "done"))
        self.assertEqual(self.req("POST", "/api/github/login/poll", {})[0], 400)  # the code is used up
        self.assertEqual(self.req("POST", "/api/github/clone", {"url": "file:///etc"})[0], 400)
        self.assertEqual(self.req("POST", "/api/github/clone", {"url": "https://github.com/octo/../x"})[0], 400)
        st, res = self.req("POST", "/api/github/clone", {"url": "https://github.com/octo/gearbox.git"})
        self.assertEqual(st, 200)
        gear = res["id"]
        doc = next(d for d in self.doc() if d["id"] == gear)
        self.assertEqual((doc["name"], doc["remote"], doc["git"]), ("gearbox", "octo/gearbox", True))
        st, res = self.req("GET", "/api/github/repos")
        by = {x["full_name"]: x for x in res["repos"]}
        self.assertEqual(by["octo/gearbox"]["doc"], gear)
        self.assertNotIn("doc", by["octo/bracket"])
        # an empty repository gets a new document, committed and pushed
        st, res = self.req("POST", "/api/github/clone", {"url": "https://github.com/octo/empty.git"})
        self.assertEqual((st, res["empty"]), (200, True))
        self.assertTrue(os.path.isfile(os.path.join(self.projects_dir, "empty", "empty.json")))
        st, res = self.req("POST", f"/api/doc/{gear}/sync", {})
        self.assertEqual((st, res["synced"]), (200, "Pushed 1 commit to octo/gearbox"))
        self.assertEqual(self.req("POST", f"/api/doc/{gear}/resolve", {"keep": "both"})[0], 400)
        self.assertEqual(self.req("POST", f"/api/doc/{gear}/resolve", {"keep": "mine"})[0], 200)
        calls = [json.loads(x) for x in open(os.path.join(self.tmp, "github.log"))]
        self.assertIn(["sync", "--doc", os.path.join(os.path.realpath(self.projects_dir), "empty", "empty.json"), "--message",
                       "Started from the rCAD phone app"], calls)
        self.assertEqual(self.req("POST", "/api/github/logout", {})[1]["signed_in"], False)

    def test_notes(self):
        did = self.doc()[0]["id"]
        st, res = self.req("POST", f"/api/doc/{did}/note", {"studio": 0, "at": [1, 2, 3], "normal": [0, 0, 1], "text": "Chamfer this edge"})
        self.assertEqual((st, res["added"]), (200, 1))
        st, d = self.req("GET", f"/api/doc/{did}")
        self.assertEqual(d["notes"], [[{"id": 1, "text": "Chamfer this edge", "at": [1, 2, 3]}]])
        self.assertEqual(self.req("POST", f"/api/doc/{did}/note", {"at": [1, 2], "text": "x"})[0], 400)
        self.assertEqual(self.req("POST", f"/api/doc/{did}/note", {"at": [1, 2, 3], "text": " "})[0], 400)
        st, res = self.req("POST", f"/api/doc/{did}/note", {"action": "edit", "id": 1, "text": "Chamfer 1 mm"})
        self.assertEqual(res["notes"][0]["text"], "Chamfer 1 mm")
        self.assertEqual(self.req("POST", f"/api/doc/{did}/note", {"action": "edit", "id": 7, "text": "?"})[0], 400)
        st, res = self.req("POST", f"/api/doc/{did}/note", {"action": "delete", "id": 1})
        self.assertEqual((st, res["notes"]), (200, []))

    def test_chat_with_photos(self):
        did = self.doc()[0]["id"]
        jpeg = base64.b64encode(b"\xff\xd8\xff\xe0" + b"x" * 100).decode()
        self.assertEqual(self.req("POST", f"/api/doc/{did}/chat", {"text": "hi", "images": ["not base64!"]})[0], 400)
        self.assertEqual(self.req("POST", f"/api/doc/{did}/chat", {"text": "hi", "images": [base64.b64encode(b"GIF89a").decode()]})[0], 400)
        self.assertEqual(self.req("POST", f"/api/doc/{did}/chat", {"text": "hi", "images": [jpeg] * 7})[0], 400)
        st, res = self.req("POST", f"/api/doc/{did}/chat", {"text": "make this", "images": [jpeg, jpeg]})
        self.assertEqual(st, 200)
        self.wait_job(res["job"])
        args = json.load(open(os.path.join(self.tmp, "chat-args.json")))
        self.assertEqual(len(args["images"]), 2)
        self.assertTrue(all(p.endswith(".jpg") for p in args["images"]))
        self.assertTrue(all(sz == 104 for sz in args["sizes"]))
        # the photos are removed once the request is done
        time.sleep(0.2)
        self.assertFalse(any(os.path.exists(p) for p in args["images"]))

    def test_new_project(self):
        st, res = self.req("POST", "/api/projects", {"name": "Hex tray"})
        self.assertEqual(st, 200)
        self.assertTrue(os.path.isfile(os.path.join(self.projects_dir, "Hex tray", "Hex tray.json")))
        self.assertIn(res["id"], [d["id"] for d in self.doc()])
        self.assertEqual(self.req("POST", "/api/projects", {"name": "Hex tray"})[0], 400)
        self.assertEqual(self.req("POST", "/api/projects", {"name": "../"})[0], 400)


if __name__ == "__main__":
    unittest.main()
