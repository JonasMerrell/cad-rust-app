#!/usr/bin/env python3
"""rCAD Capture receiver: takes photogrammetry photos from the rCAD Capture
Android app and stores them as one folder per capture session.

Layout (read by rCAD's photogrammetry import):

    <root>/<YYYY-MM-DD_HHMM>-<name>/IMG_0001.jpg ...
    <root>/<YYYY-MM-DD_HHMM>-<name>/session.json

session.json has "complete": false while photos are still arriving.

Standard library only. Run:

    python3 rcad_capture_receiver.py serve [--root ~/scans] [--port 8765]
    python3 rcad_capture_receiver.py pair-url [--host 192.168.0.247]   # QR this link

The bearer token lives in ~/.config/rcad-capture/token (made on first run).
"""

import argparse
import hashlib
import html
import json
import os
import re
import secrets
import shutil
import socket
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:  # projects, web viewer and Claude chats for the rCAD phone app (installed next to this file)
    import rcad_projects
except ImportError:
    rcad_projects = None

VERSION = "1.1"
CONFIG_DIR = os.path.expanduser("~/.config/rcad-capture")
TOKEN_FILE = os.path.join(CONFIG_DIR, "token")
# Served at /rcad-capture.apk so the phone can install the app from netdash.
APK_FILE = os.path.expanduser("~/.local/share/rcad-capture/rcad-capture.apk")
# The rCAD phone app (viewer + Claude chat), served at /rcad.apk.
MOBILE_APK_FILE = os.path.expanduser("~/.local/share/rcad-capture/rcad.apk")
MAX_PHOTO_BYTES = 200 * 1024 * 1024
PHOTO_EXT = (".jpg", ".jpeg", ".png", ".dng")
RESERVED = {"rcad-scan", "session.json"}
CAL_PREFIX = "CAL_"  # calibration photos: the known-size reference object is in view

_lock = threading.Lock()


def load_token(create=True):
    try:
        with open(TOKEN_FILE) as f:
            tok = f.read().strip()
            if tok:
                return tok
    except FileNotFoundError:
        pass
    if not create:
        return None
    os.makedirs(CONFIG_DIR, exist_ok=True)
    tok = secrets.token_urlsafe(24)
    fd = os.open(TOKEN_FILE, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(tok + "\n")
    return tok


def slug(text, fallback="scan", limit=48):
    s = re.sub(r"[^A-Za-z0-9._-]+", "-", (text or "").strip()).strip("-._")
    return (s[:limit].strip("-._") or fallback)


def safe_photo_name(name):
    base = os.path.basename(name or "")
    stem, ext = os.path.splitext(base)
    if ext.lower() not in PHOTO_EXT:
        return None
    stem = slug(stem, fallback="")
    if not stem:
        return None
    return stem + ext.lower()


def safe_session_id(sid):
    if not sid or sid in RESERVED or not re.fullmatch(r"[A-Za-z0-9._-]{1,80}", sid) or sid.startswith("."):
        return None
    return sid


def read_json(path):
    with open(path) as f:
        return json.load(f)


def write_json_atomic(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)


def list_photos(folder):
    out = []
    for n in sorted(os.listdir(folder)):
        p = os.path.join(folder, n)
        if os.path.isfile(p) and os.path.splitext(n)[1].lower() in PHOTO_EXT:
            out.append(n)
    return out


class App:
    def __init__(self, root, token, extra_urls=(), projects=None):
        self.extra_urls = list(extra_urls)
        self.projects = projects
        self.root = os.path.abspath(os.path.expanduser(root))
        self.token = token
        os.makedirs(self.root, exist_ok=True)

    def session_dir(self, sid):
        sid = safe_session_id(sid)
        if sid is None:
            return None
        d = os.path.join(self.root, sid)
        if not os.path.isfile(os.path.join(d, "session.json")):
            return None
        return d

    def create_session(self, body):
        name = slug(body.get("name"), fallback="scan")
        stamp = slug(body.get("stamp") or time.strftime("%Y-%m-%d_%H%M"), fallback=time.strftime("%Y-%m-%d_%H%M"), limit=20)
        client_id = str(body.get("client_id") or "")[:80]
        with _lock:
            # The phone retries a create it never got an answer for: reuse the folder.
            if client_id:
                for n in os.listdir(self.root):
                    sj = os.path.join(self.root, n, "session.json")
                    if os.path.isfile(sj):
                        try:
                            if read_json(sj).get("client_id") == client_id:
                                return n
                        except (OSError, ValueError):
                            pass
            base = f"{stamp}-{name}"
            sid, i = base, 2
            while os.path.exists(os.path.join(self.root, sid)):
                sid = f"{base}-{i}"
                i += 1
            d = os.path.join(self.root, sid)
            os.makedirs(d)
            ruler = body.get("ruler_mm")
            meta = {
                "name": body.get("name") or name,
                "created": body.get("created") or time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "device": body.get("device"),
                "notes": body.get("notes") or "",
                "ruler_mm": float(ruler) if isinstance(ruler, (int, float)) else None,
                "photo_count": 0,
                "complete": False,
                "camera": body.get("camera") or {},
                "reference": clean_reference(body.get("reference")),
                "calibration_photos": [],
                "client_id": client_id or None,
                "app": body.get("app"),
            }
            write_json_atomic(os.path.join(d, "session.json"), meta)
        return sid

    def finish_session(self, d, body):
        with _lock:
            sj = os.path.join(d, "session.json")
            meta = read_json(sj)
            photos = list_photos(d)
            meta["photo_count"] = len(photos)
            expected = body.get("photo_count")
            if isinstance(expected, int) and expected != len(photos):
                return False, {"error": f"expected {expected} photos, have {len(photos)}", "have": photos}
            if isinstance(body.get("camera"), dict):
                meta["camera"] = {**meta.get("camera", {}), **body["camera"]}
            if body.get("notes"):
                meta["notes"] = body["notes"]
            if body.get("reference"):
                meta["reference"] = clean_reference(body["reference"])
            ruler = body.get("ruler_mm")
            if isinstance(ruler, (int, float)):
                meta["ruler_mm"] = float(ruler)
            meta["calibration_photos"] = [n for n in photos if n.startswith(CAL_PREFIX)]
            if isinstance(body.get("captured"), int):
                meta["captured"] = body["captured"]
            meta["complete"] = True
            meta["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
            write_json_atomic(sj, meta)
            return True, meta

    def bump_count(self, d):
        with _lock:
            sj = os.path.join(d, "session.json")
            meta = read_json(sj)
            photos = list_photos(d)
            meta["photo_count"] = len(photos)
            meta["calibration_photos"] = [n for n in photos if n.startswith(CAL_PREFIX)]
            write_json_atomic(sj, meta)


def clean_reference(ref):
    """{"kind": "123-block"|"246-block"|"ruler"|"box"|"sphere", "label", "dims_mm": [..]} or None."""
    if not isinstance(ref, dict):
        return None
    kind = slug(str(ref.get("kind") or "box"), fallback="box", limit=24)
    dims = [float(v) for v in (ref.get("dims_mm") or []) if isinstance(v, (int, float)) and v > 0][:3]
    out = {"kind": kind, "label": str(ref.get("label") or kind)[:80], "dims_mm": dims}
    if ref.get("notes"):
        out["notes"] = str(ref["notes"])[:200]
    return out


class Handler(BaseHTTPRequestHandler):
    server_version = "rcad-capture/" + VERSION
    protocol_version = "HTTP/1.1"
    app: App = None  # set by serve()

    def log_message(self, fmt, *args):
        # the viewer passes the token in the query (?t=…): keep it out of the journal
        sys.stderr.write("%s %s\n" % (self.address_string(), re.sub(r"([?&]t=)[^&\s\"]+", r"\1…", fmt % args)))

    def log_request(self, code="-", size="-"):
        # logged once the response is sent, with how long it took (see handle_one_request)
        self._logged = (code, size)

    def parse_request(self):
        # the request line is in: time from here (not from waiting on a kept-alive connection)
        self._t0 = time.monotonic()
        return super().parse_request()

    def handle_one_request(self):
        self._t0, self._logged = time.monotonic(), None
        try:
            super().handle_one_request()
        except (ConnectionResetError, BrokenPipeError):
            # the phone dropped a connection (an idle keep-alive one, or it gave up on a page): nothing to report
            self.close_connection = True
            return
        if self._logged:
            code, size = self._logged
            code = getattr(code, "value", code)
            self.log_message('"%s" %s %s %d ms', self.requestline, code, size, (time.monotonic() - self._t0) * 1000)

    # -- helpers
    def send_json(self, code, obj):
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def authed(self):
        auth = self.headers.get("Authorization", "")
        ok = auth.startswith("Bearer ") and secrets.compare_digest(auth[7:].strip(), self.app.token)
        if not ok and self.command == "GET":
            # the phone app's web view loads viewer pages by URL, with the token in the query
            t = (urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get("t") or [""])[0]
            ok = bool(t) and secrets.compare_digest(t, self.app.token)
        if not ok:
            self.drain()
            self.send_json(401, {"error": "bad or missing token"})
        return ok

    def length(self):
        try:
            return int(self.headers.get("Content-Length", "0"))
        except ValueError:
            return -1

    def drain(self):
        n = self.length()
        while n > 0:
            chunk = self.rfile.read(min(n, 1 << 20))
            if not chunk:
                break
            n -= len(chunk)

    def body_json(self):
        n = self.length()
        if n < 0 or n > 1 << 20:
            raise ValueError("bad length")
        raw = self.rfile.read(n) if n else b"{}"
        obj = json.loads(raw or b"{}")
        if not isinstance(obj, dict):
            raise ValueError("expected an object")
        return obj

    def parts(self):
        path = urllib.parse.urlparse(self.path).path
        return [urllib.parse.unquote(p) for p in path.strip("/").split("/") if p]

    def send_file(self, path, ctype, name=None):
        try:
            f = open(path, "rb")
        except OSError:
            return self.send_json(404, {"error": "not found"})
        with f:
            size = os.fstat(f.fileno()).st_size
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(size))
            if name:
                self.send_header("Content-Disposition", f'attachment; filename="{name}"')
            self.end_headers()
            shutil.copyfileobj(f, self.wfile, 1 << 20)

    def send_html(self, code, html):
        data = html.encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def pair_page(self):
        """/pair?t=<token>: a QR code of this URL pairs the phone (any QR scanner opens http links)."""
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        tok = (q.get("t") or [""])[0]
        if not secrets.compare_digest(tok, self.app.token):
            return self.send_html(401, "<h1>Wrong pairing link</h1>")
        host = self.headers.get("Host") or f"{primary_ip()}:{self.server.server_address[1]}"
        urls = [f"http://{host}"] + [u for u in self.app.extra_urls if u != f"http://{host}"]
        raw = pair_url_from(urls, tok)
        link = html.escape(raw, quote=True)
        mlink = html.escape(raw.replace("rcadcapture://", "rcad://", 1), quote=True)
        js_link = json.dumps(raw).replace("<", "\\u003c")
        self.send_html(200, f"""<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>Pair rCAD Capture</title>
<body style="font-family:sans-serif;text-align:center;padding:40px 16px;background:#111;color:#eee">
<h2>Pair rCAD Capture with {html.escape(socket.gethostname())}</h2>
<p><a href="{link}" style="display:inline-block;padding:18px 28px;background:#1f6feb;color:#fff;border-radius:12px;text-decoration:none;font-size:20px">Open in rCAD Capture</a></p>
<p><a href="{mlink}" style="display:inline-block;padding:18px 28px;background:#2d333b;color:#fff;border-radius:12px;text-decoration:none;font-size:20px">Open in rCAD</a></p>
<p style="color:#999">Install the app first. Photos will upload to {html.escape(self.app.root)}.</p>
<script>setTimeout(function(){{location.href={js_link}}},300)</script>""")

    # -- routes
    def do_GET(self):
        p = self.parts()
        if p == ["pair"]:
            return self.pair_page()
        if p == []:
            have = os.path.isfile(APK_FILE)
            dl = ('<p><a href="/rcad-capture.apk" style="display:inline-block;padding:18px 28px;background:#1f6feb;'
                  'color:#fff;border-radius:12px;text-decoration:none;font-size:20px">Download rCAD Capture</a></p>'
                  if have else "<p>No rCAD Capture APK on this server yet.</p>")
            if os.path.isfile(MOBILE_APK_FILE):
                dl += ('<p><a href="/rcad.apk" style="display:inline-block;padding:18px 28px;background:#2d333b;'
                       'color:#fff;border-radius:12px;text-decoration:none;font-size:20px">Download rCAD (viewer + Claude)</a></p>')
            return self.send_html(200, f"""<!doctype html><meta name=viewport content="width=device-width,initial-scale=1">
<title>rCAD Capture</title>
<body style="font-family:sans-serif;text-align:center;padding:40px 16px;background:#111;color:#eee">
<h2>rCAD Capture on {html.escape(socket.gethostname())}</h2>{dl}
<p style="color:#999">Then open the pairing link (or scan its QR code) to connect the app.</p>""")
        if p == ["favicon.ico"]:
            # browsers and the app's web view ask for this on their own; answer without a token
            # (a 401 makes Chromium ask again on every redraw)
            self.send_response(204)
            self.send_header("Cache-Control", "max-age=86400")
            self.end_headers()
            return
        if p == ["rcad.apk"]:
            return self.send_file(MOBILE_APK_FILE, "application/vnd.android.package-archive", "rcad.apk")
        if p == ["rcad-capture.apk"]:
            try:
                with open(APK_FILE, "rb") as f:
                    data = f.read()
            except OSError:
                return self.send_json(404, {"error": "no APK on this server"})
            self.send_response(200)
            self.send_header("Content-Type", "application/vnd.android.package-archive")
            self.send_header("Content-Disposition", 'attachment; filename="rcad-capture.apk"')
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if p == ["api", "ping"]:
            if not self.authed():
                return
            du = shutil.disk_usage(self.app.root)
            return self.send_json(200, {"ok": True, "version": VERSION, "host": socket.gethostname(),
                                        "root": self.app.root, "free_bytes": du.free,
                                        "projects": bool(self.app.projects)})
        if not self.authed():
            return
        if p == ["api", "sessions"]:
            out = []
            for n in sorted(os.listdir(self.app.root), reverse=True):
                sj = os.path.join(self.app.root, n, "session.json")
                if os.path.isfile(sj):
                    try:
                        m = read_json(sj)
                    except (OSError, ValueError):
                        continue
                    out.append({"id": n, "name": m.get("name"), "photo_count": m.get("photo_count"),
                                "complete": m.get("complete"), "created": m.get("created")})
            return self.send_json(200, {"sessions": out})
        if len(p) == 3 and p[:2] == ["api", "sessions"]:
            d = self.app.session_dir(p[2])
            if not d:
                return self.send_json(404, {"error": "no such session"})
            return self.send_json(200, {"id": p[2], "session": read_json(os.path.join(d, "session.json")),
                                        "photos": list_photos(d)})
        if self.app.projects and rcad_projects.handle(self, "GET", p, self.app.projects):
            return
        self.send_json(404, {"error": "not found"})

    def do_POST(self):
        if not self.authed():
            return
        p = self.parts()
        try:
            body = self.body_json()
        except ValueError as e:
            return self.send_json(400, {"error": str(e)})
        if p == ["api", "sessions"]:
            sid = self.app.create_session(body)
            return self.send_json(200, {"id": sid, "path": os.path.join(self.app.root, sid)})
        if len(p) == 4 and p[:2] == ["api", "sessions"] and p[3] == "finish":
            d = self.app.session_dir(p[2])
            if not d:
                return self.send_json(404, {"error": "no such session"})
            ok, res = self.app.finish_session(d, body)
            return self.send_json(200 if ok else 409, res)
        if self.app.projects and rcad_projects.handle(self, "POST", p, self.app.projects, body):
            return
        self.send_json(404, {"error": "not found"})

    def do_PUT(self):
        if not self.authed():
            return
        p = self.parts()
        if not (len(p) == 5 and p[:2] == ["api", "sessions"] and p[3] == "photos"):
            self.drain()
            return self.send_json(404, {"error": "not found"})
        d = self.app.session_dir(p[2])
        name = safe_photo_name(p[4])
        n = self.length()
        if not d or not name:
            self.drain()
            return self.send_json(404 if not d else 400, {"error": "no such session" if not d else "bad file name"})
        if n <= 0 or n > MAX_PHOTO_BYTES:
            self.drain()
            return self.send_json(413 if n > 0 else 411, {"error": "bad size"})
        want = (self.headers.get("X-Content-SHA256") or "").lower()
        final = os.path.join(d, name)
        tmp = os.path.join(d, f".{name}.{secrets.token_hex(4)}.part")
        h = hashlib.sha256()
        got = 0
        try:
            with open(tmp, "wb") as f:
                while got < n:
                    chunk = self.rfile.read(min(n - got, 1 << 20))
                    if not chunk:
                        break
                    f.write(chunk)
                    h.update(chunk)
                    got += len(chunk)
                f.flush()
                os.fsync(f.fileno())
            digest = h.hexdigest()
            if got != n:
                os.unlink(tmp)
                return self.send_json(400, {"error": f"short body {got}/{n}"})
            if want and want != digest:
                os.unlink(tmp)
                return self.send_json(422, {"error": "sha256 mismatch", "sha256": digest})
            os.replace(tmp, final)
        except OSError as e:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            return self.send_json(507, {"error": str(e)})
        self.app.bump_count(d)
        self.send_json(200, {"ok": True, "name": name, "size": got, "sha256": digest})


def primary_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def pair_url_from(urls, token):
    return "rcadcapture://pair?" + urllib.parse.urlencode({"url": ",".join(urls), "token": token})


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve")
    s.add_argument("--root", default="~/scans")
    s.add_argument("--bind", default="0.0.0.0")
    s.add_argument("--port", type=int, default=8765)
    s.add_argument("--also-url", action="append", default=[],
                   help="extra receiver URL the phone falls back to (e.g. over WireGuard); repeatable")
    s.add_argument("--projects", action="append", default=[],
                   help="rCAD project folder for the phone app (default: RCAD_PROJECTS, ~/Documents/rCAD Projects, ~/rCAD Projects); repeatable")
    s.add_argument("--rcad", help="the rcad program (default: RCAD_BIN, PATH, ~/src/cad-rust-app/target/release/rcad)")
    s.add_argument("--model", help="Claude model for phone chats when the phone doesn't pick one (default: the editor's setting)")
    s.add_argument("--auth", choices=["claude-code", "api-key", "console"],
                   help="how phone chats sign in to Claude (default claude-code: this computer's Claude Code login)")
    q = sub.add_parser("pair-url", help="print the http pairing link; make a QR code of it and scan it with the phone")
    q.add_argument("--host", help="address the phone uses (default: this machine's LAN address)")
    q.add_argument("--port", type=int, default=8765)
    a = ap.parse_args(argv)
    token = load_token()
    if a.cmd == "pair-url":
        print(f"http://{a.host or primary_ip()}:{a.port}/pair?t={urllib.parse.quote(token)}")
        return 0
    projects = rcad_projects.Projects(a.projects or None, a.rcad, a.model, a.auth) if rcad_projects else None
    Handler.app = App(a.root, token, a.also_url, projects)
    srv = ThreadingHTTPServer((a.bind, a.port), Handler)
    srv.daemon_threads = True
    print(f"rcad-capture {VERSION} on {a.bind}:{a.port}, saving to {Handler.app.root}", flush=True)
    if projects:
        print(f"rCAD projects in {', '.join(projects.roots)}; rcad: {projects.rcad or 'not found'}", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
