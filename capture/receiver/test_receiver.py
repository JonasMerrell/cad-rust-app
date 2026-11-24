"""Tests for rcad_capture_receiver (python3 -m unittest capture/receiver/test_receiver.py)."""
import hashlib
import http.client
import json
import os
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import rcad_capture_receiver as r  # noqa: E402


class ReceiverTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        r.Handler.app = r.App(self.tmp, "tok")
        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), r.Handler)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.port = self.srv.server_address[1]

    def tearDown(self):
        self.srv.shutdown()

    def req(self, method, path, body=None, token="tok", headers=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        h = dict(headers or {})
        if token:
            h["Authorization"] = "Bearer " + token
        if isinstance(body, dict):
            body = json.dumps(body).encode()
        c.request(method, path, body=body, headers=h)
        resp = c.getresponse()
        data = json.loads(resp.read() or b"{}")
        c.close()
        return resp.status, data

    def test_pair_page(self):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.request("GET", "/pair?t=tok")
        r1 = c.getresponse()
        body = r1.read().decode()
        self.assertEqual(r1.status, 200)
        self.assertIn("rcadcapture://pair?url=http%3A%2F%2F127.0.0.1", body)
        script = body[body.index("<script>"):]
        self.assertIn("&token=tok", script)
        self.assertNotIn("&amp;", script)
        c.request("GET", "/pair?t=nope")
        r2 = c.getresponse()
        r2.read()
        self.assertEqual(r2.status, 401)

    def test_favicon_needs_no_token(self):
        # a 401 here made the app's web view ask again on every redraw
        status, _ = self.req("GET", "/favicon.ico", token=None)
        self.assertEqual(status, 204)

    def test_flow(self):
        self.assertEqual(self.req("GET", "/api/ping", token="bad")[0], 401)
        self.assertEqual(self.req("GET", "/api/ping")[0], 200)
        st, s = self.req("POST", "/api/sessions", {"name": "red mug", "stamp": "2026-11-21_1443", "client_id": "abc", "ruler_mm": 100})
        self.assertEqual(st, 200)
        sid = s["id"]
        self.assertEqual(sid, "2026-11-21_1443-red-mug")
        # retried create reuses the folder
        self.assertEqual(self.req("POST", "/api/sessions", {"name": "red mug", "client_id": "abc"})[1]["id"], sid)
        img = os.urandom(5000)
        sha = hashlib.sha256(img).hexdigest()
        st, res = self.req("PUT", f"/api/sessions/{sid}/photos/IMG_0001.jpg", img, headers={"X-Content-SHA256": sha})
        self.assertEqual((st, res["sha256"]), (200, sha))
        st, _ = self.req("PUT", f"/api/sessions/{sid}/photos/IMG_0002.jpg", img, headers={"X-Content-SHA256": "0" * 64})
        self.assertEqual(st, 422)
        self.assertEqual(self.req("PUT", f"/api/sessions/{sid}/photos/..%2Fevil.jpg", img)[0], 200)  # basename only
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "evil.jpg")))
        self.assertEqual(self.req("PUT", f"/api/sessions/{sid}/photos/x.sh", img)[0], 400)
        self.assertEqual(self.req("PUT", "/api/sessions/..%2F..%2Ftmp/photos/a.jpg", img)[0], 404)
        self.assertEqual(self.req("POST", f"/api/sessions/{sid}/finish", {"photo_count": 5})[0], 409)
        self.assertEqual(self.req("PUT", f"/api/sessions/{sid}/photos/CAL_0003.jpg", img)[0], 200)
        st, meta = self.req("POST", f"/api/sessions/{sid}/finish", {"photo_count": 3, "reference": {"kind": "123-block", "dims_mm": [25.4, 50.8, 76.2]}})
        self.assertEqual(st, 200)
        self.assertTrue(meta["complete"])
        self.assertEqual(meta["ruler_mm"], 100.0)
        self.assertEqual(meta["calibration_photos"], ["CAL_0003.jpg"])
        self.assertEqual(meta["reference"]["dims_mm"], [25.4, 50.8, 76.2])
        files = sorted(os.listdir(os.path.join(self.tmp, sid)))
        self.assertEqual(files, ["CAL_0003.jpg", "IMG_0001.jpg", "evil.jpg", "session.json"])
        self.assertEqual(self.req("GET", "/api/sessions")[1]["sessions"][0]["photo_count"], 3)


if __name__ == "__main__":
    unittest.main()
