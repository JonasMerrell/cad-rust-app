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
                open(args["path"], "w").write("<html>%s %s</html>" % (doc["studios"][0]["name"], json.dumps(args)))
            print(json.dumps({"tool": t, "ok": True}))
        sys.exit(0)
    if a[0] == "chat":
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


class ProjectsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.projects_dir = os.path.join(self.tmp, "rCAD Projects")
        os.makedirs(os.path.join(self.projects_dir, "Bracket", ".rcad-cache"))
        with open(os.path.join(self.projects_dir, "Bracket", "bracket.json"), "w") as f:
            json.dump({"format": "cad-rust-app/workspace-1", "studios": [{"name": "Body", "doc": {}}],
                       "assemblies": [{"name": "Arm"}]}, f, indent=2)
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
        # the web view takes the token in the query too
        st, page = self.req("GET", f"/api/doc/{did}/view?t=tok", token=None, raw=True)
        self.assertEqual(st, 200)
        self.assertIn("Body", page)
        st, page = self.req("GET", f"/api/doc/{did}/view?assembly=0", raw=True)
        self.assertIn('"assembly": 0', page)
        self.assertEqual(self.req("GET", f"/api/doc/{did}/view?t=bad", token=None)[0], 401)

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

    def test_new_project(self):
        st, res = self.req("POST", "/api/projects", {"name": "Hex tray"})
        self.assertEqual(st, 200)
        self.assertTrue(os.path.isfile(os.path.join(self.projects_dir, "Hex tray", "Hex tray.json")))
        self.assertIn(res["id"], [d["id"] for d in self.doc()])
        self.assertEqual(self.req("POST", "/api/projects", {"name": "Hex tray"})[0], 400)
        self.assertEqual(self.req("POST", "/api/projects", {"name": "../"})[0], 400)


if __name__ == "__main__":
    unittest.main()
