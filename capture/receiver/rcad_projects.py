"""rCAD projects for the phone app: list documents on netdash, show them in rCAD's web viewer,
and chat with Claude about them.

Part of the netdash receiver (rcad_capture_receiver.py serves these routes on the same port and
token as the Capture app). Standard library only.

    GET  /api/projects                      documents in the project folders
    POST /api/projects                      {"name"}: a new project with an empty document
    GET  /api/doc/<id>                      one document: tabs, each Part Studio's feature tree, chats,
                                            whether Claude is busy on it
    GET  /api/doc/<id>/view[?<kind>=N][&page=P][&t=<token>]
                                            a page showing one tab, cached: kind studio (default) or
                                            assembly (export_web), cam (export_cam_simulation; page
                                            "sheet" the setup sheet), drawing (a sheet's SVG; page
                                            the sheet number), simulation (solved in the background:
                                            a "solving" page that reloads itself until the result
                                            pictures are ready), pcb (board top and bottom,
                                            schematic and circuit waveforms; page "3d" the board
                                            in 3D)
    GET  /api/doc/<id>/tree[?<kind>=N][&page=P]
                                            what the phone lists beside that page: the feature tree,
                                            an assembly's instances and mates, or (other kinds)
                                            "sections" of rows plus the tab's other "pages"
    GET  /api/doc/<id>/chats/<chat>         a saved chat (the same .rcad-chats the editor uses)
    POST /api/doc/<id>/chat                 {"text", "chat_id"?, "studio"?, "model"?, "effort"?}
                                            starts a request: {"job", "chat_id"}
    GET  /api/jobs/<job>?after=N            events after N (waits up to 25 s for new ones)
    POST /api/jobs/<job>/stop
    POST /api/doc/<id>/sync                 commit (when the project commits on save) and sync with GitHub
    POST /api/doc/<id>/resolve              {"keep": "mine"|"theirs"} after a sync that couldn't combine both sides

    GET  /api/github                        the GitHub account rCAD on netdash is signed in with
    POST /api/github/login                  start signing in: {"user_code", "verification_uri", "expires_in", "interval"}
    POST /api/github/login/poll             {"state": "waiting"|"slow_down"|"done", "login"}
    POST /api/github/logout
    GET  /api/github/repos                  the account's repositories, rCAD projects first; each with
                                            "doc" (its document id) when a copy is already on netdash
    POST /api/github/clone                  {"url"}: clone into the first project folder: {"id"}

GitHub goes through `rcad github` (src/github_cli.rs), the editor's own sign-in and git code, so
the token stays on netdash and signing in here signs the editor in too. After a phone chat changes
a document in a git project, `rcad chat` commits and syncs it as the editor does after a save.

Requests run `rcad chat` (src/claude_cli.rs), which uses this computer's Claude Code login, so the
phone never holds a Claude sign-in or key. Document ids are the path relative to a project folder,
base64url-encoded, prefixed by the folder's index.
"""

import base64
import gzip
import hashlib
import json
import os
import re
import secrets
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.parse

DOC_MARK = b'"cad-rust-app/'
SKIP_DIRS = {"target", "node_modules", "__pycache__", "rcad-scan"}
MAX_DEPTH = 4
CACHE_DIR = os.path.expanduser("~/.cache/rcad-mobile")
JOB_KEEP_SECS = 3600
EXPORT_TIMEOUT = 15 * 60
# a background build has no request waiting on it: give a first import of a big STEP file time
# (it is stored in rCAD's build cache as soon as it is built, so it is slow only once)
BACKGROUND_TIMEOUT = 60 * 60
# bump when what a cached page holds changes
VIEW_VERSION = 5
# tab kinds a page can show, by query parameter, and their list in the workspace file
TAB_KINDS = {"studio": "studios", "assembly": "assemblies", "cam": "cams", "drawing": "drawings", "simulation": "simulations", "pcb": "pcbs"}
# kinds whose page can take minutes (an FEA solve, a big assembly opened the first time): built in the
# background while the phone shows progress. A request waits QUICK_WAIT seconds first, so a page that
# builds quickly comes back as it is.
BACKGROUND_KINDS = {"simulation", "assembly", "studio"}
QUICK_WAIT = 8
# kinds whose pages all come from one rcad run (a Simulation tab's 3D page and its pictures)
SHARED_BUILD_KINDS = {"simulation"}
# feature params worth showing in the tree, in this order
SHOWN_PARAMS = ("depth", "radius", "distance", "thickness", "angle", "count", "count2", "offset", "pitch", "height", "end", "op")
# a phone's web view chokes on millions of triangles (a big assembly's page was 120 MB): 3D pages
# are thinned to about this many (export_web max_triangles; sharp edges stay)
PHONE_TRIANGLES = 750_000
# pages at least this big go out gzipped (a big assembly's page is half the size)
GZIP_MIN = 64 * 1024
MODELS = {"opus": "claude-opus-5-5", "sonnet": "claude-sonnet-5-5", "haiku": "claude-haiku-5-5"}
EFFORTS = {"low", "medium", "high", "xhigh", "max"}


def default_roots():
    """rCAD's own projects folder (RCAD_PROJECTS, else "rCAD Projects" in Documents or home)."""
    home = os.path.expanduser("~")
    docs, plain = os.path.join(home, "Documents", "rCAD Projects"), os.path.join(home, "rCAD Projects")
    # the first is where new projects go: the same folder the editor uses (src/project.rs), which
    # is in Documents only when that folder exists; the others are still listed
    first = plain if not os.path.isdir(os.path.join(home, "Documents")) else docs
    out = [os.environ["RCAD_PROJECTS"]] if os.environ.get("RCAD_PROJECTS") else []
    for d in (first, docs, plain):
        if d not in out:
            out.append(d)
    return out


def find_rcad(explicit=None):
    cands = [explicit, os.environ.get("RCAD_BIN"), shutil.which("rcad"), shutil.which("cad-rust-app"),
             os.path.expanduser("~/src/cad-rust-app/target/release/rcad"),
             os.path.expanduser("~/.local/bin/rcad")]
    for c in cands:
        if c and os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return None


def doc_id(root_index, rel):
    return f"{root_index}-" + base64.urlsafe_b64encode(rel.encode()).decode().rstrip("=")


def is_doc(path):
    try:
        with open(path, "rb") as f:
            head = f.read(4096)
    except OSError:
        return False
    return head.lstrip().startswith(b"{") and DOC_MARK in head


LENGTH_KEYS = ("depth", "radius", "distance", "thickness", "offset", "height", "length", "width")


def _num(v, unit=""):
    """A parameter as the editor shows it: an expression as typed, a plain number with its unit."""
    v = str(v).strip()
    try:
        float(v)
        return v + unit
    except ValueError:
        return v


def feature_detail(op):
    """A short line about one feature (what the tree shows under its name)."""
    if not isinstance(op, dict):
        return ""
    t = op.get("type", "")
    out = []
    if t == "sketch":
        pl = op.get("plane") or {}
        if pl.get("kind") == "origin_plane":
            out.append(f"{pl.get('plane', '')} plane")
        elif pl.get("kind"):
            out.append("on " + str(pl.get("kind")).replace("_", " "))
        sk = op.get("sketch") or {}
        n = len(sk.get("curves") or []) + len(sk.get("texts") or [])
        if n:
            out.append(f"{n} curve{'s' if n != 1 else ''}")
        c = len(sk.get("constraints") or [])
        if c:
            out.append(f"{c} constraint{'s' if c != 1 else ''}")
        return " · ".join(out)
    for k in LENGTH_KEYS:
        if op.get(k) not in (None, ""):
            v = _num(op[k], " mm")
            out.append(v if v.endswith(" mm") else f"{k} {v}")  # an expression: say what it sets
            break
    if op.get("angle") not in (None, "") and t in ("revolve", "circularPattern", "draft", "surfaceRevolve"):
        out.append(_num(op["angle"], "°"))
    if op.get("count") not in (None, ""):
        out.append(f"× {_num(op['count'])}")
    if t == "extrude" and op.get("end") not in (None, "", "blind"):
        out.append(str(op["end"]).replace("_", " "))
    if op.get("op") in ("remove", "intersect") or (op.get("op") == "new" and t != "boolean"):
        out.append({"remove": "cut", "intersect": "intersect", "new": "new part"}[op["op"]])
    for k in ("edges", "faces", "bodies"):
        v = op.get(k)
        if isinstance(v, list) and v:
            out.append(f"{len(v)} {k[:-1] if len(v) == 1 else k}")
            break
    return " · ".join(out)


def feature_tree(doc):
    """One Part Studio's feature list, rollback bar and variables, for the phone's tree."""
    if not isinstance(doc, dict):
        return {"features": [], "rollback": None, "variables": []}
    feats = []
    for f in doc.get("features") or []:
        if not isinstance(f, dict):
            continue
        op = f.get("op") if isinstance(f.get("op"), dict) else {}
        feats.append({
            "id": f.get("id"),
            "name": f.get("name", ""),
            "type": op.get("type", ""),
            "detail": feature_detail(op),
            "suppressed": bool(f.get("suppressed")),
            "hidden": bool(f.get("hidden")),
        })
    rb = doc.get("rollback")
    variables = [{"name": v.get("name", ""), "expr": str(v.get("expr", ""))}
                 for v in doc.get("variables") or [] if isinstance(v, dict)]
    return {"features": feats, "rollback": rb if isinstance(rb, int) else None, "variables": variables}


class Job:
    def __init__(self, jid, doc, proc, chat_id, prompt=""):
        self.id = jid
        self.prompt = prompt
        self.doc = doc
        self.proc = proc
        self.chat_id = chat_id
        self.events = []
        self.done = False
        self.error = None
        self.finished_at = None
        self.cond = threading.Condition()

    def add(self, ev):
        with self.cond:
            if ev.get("type") == "chat" and ev.get("id"):
                self.chat_id = ev["id"]
            self.events.append(ev)
            self.cond.notify_all()

    def finish(self, error):
        with self.cond:
            if not any(e.get("type") == "done" for e in self.events):
                self.events.append({"type": "done", "error": error})
            self.done = True
            self.error = error if error else next((e.get("error") for e in self.events if e.get("type") == "done"), None)
            self.finished_at = time.time()
            self.cond.notify_all()

    def since(self, after, wait):
        with self.cond:
            if len(self.events) <= after and not self.done and wait > 0:
                self.cond.wait(wait)
            return self.events[after:], len(self.events), self.done


class Projects:
    def __init__(self, roots=None, rcad=None, model=None, auth=None):
        roots = roots or default_roots()
        self.roots = [os.path.abspath(os.path.expanduser(r)) for r in roots]
        self.rcad_hint = rcad
        self.model = model
        self.auth = auth  # None: Claude Code (rcad chat's default)
        self.jobs = {}
        self.busy = {}  # doc path -> job id
        self.lock = threading.Lock()
        self.info_cache = {}  # path -> (mtime, size, info)
        self.export_locks = {}
        self.building = {}  # cache key -> start time, for background builds
        self.build_done = {}  # cache key -> Event set when its background build ends
        self.build_errors = {}  # cache key -> error of a background build that failed
        self.gh_status = None  # (time, `rcad github status`)
        self.gh_device = None  # (device code, expiry) of a sign-in under way

    @property
    def rcad(self):
        return find_rcad(self.rcad_hint)

    # ---------------------------------------------------------------- documents

    def resolve(self, did):
        """The document path of an id, or None (ids never reach outside the project folders)."""
        m = re.fullmatch(r"(\d+)-([A-Za-z0-9_-]+)", did or "")
        if not m:
            return None
        i = int(m.group(1))
        if i >= len(self.roots):
            return None
        try:
            rel = base64.urlsafe_b64decode(m.group(2) + "=" * (-len(m.group(2)) % 4)).decode()
        except (ValueError, UnicodeDecodeError):
            return None
        root = os.path.realpath(self.roots[i])
        path = os.path.realpath(os.path.join(root, rel))
        if not path.startswith(root + os.sep) or not path.endswith(".json") or not os.path.isfile(path):
            return None
        return path

    def scan(self):
        out = []
        for i, root in enumerate(self.roots):
            if not os.path.isdir(root):
                continue
            base_depth = root.rstrip(os.sep).count(os.sep)
            for d, dirs, files in os.walk(root):
                dirs[:] = sorted(x for x in dirs if not x.startswith(".") and x not in SKIP_DIRS)
                if d.count(os.sep) - base_depth >= MAX_DEPTH:
                    dirs[:] = []
                for f in sorted(files):
                    p = os.path.join(d, f)
                    if f.endswith(".json") and not f.startswith(".") and is_doc(p):
                        out.append((i, os.path.relpath(p, root), p))
        return out

    def info(self, path):
        """Tab names of a document (read once per change)."""
        try:
            st = os.stat(path)
        except OSError:
            return None
        hit = self.info_cache.get(path)
        if hit and hit[0] == st.st_mtime_ns and hit[1] == st.st_size:
            return hit[2]
        try:
            with open(path) as f:
                ws = json.load(f)
        except (OSError, ValueError):
            return None
        if "studios" in ws:
            studios = [s for s in ws.get("studios") or [] if isinstance(s, dict)]
            info = {
                "studios": [s.get("name", f"Part Studio {n + 1}") for n, s in enumerate(studios)],
                "assemblies": [a.get("name", f"Assembly {n + 1}") for n, a in enumerate(ws.get("assemblies") or [])],
                "drawings": _names(ws.get("drawings"), "Drawing"),
                "trees": [feature_tree(s.get("doc")) for s in studios],
                "cams": _names(ws.get("cams"), "Setup"),
                "simulations": _names(ws.get("simulations"), "Simulation"),
                "pcbs": _names(ws.get("pcbs"), "PCB"),
            }
        else:  # an old single Part Studio document
            info = {"studios": ["Part Studio 1"], "assemblies": [], "drawings": [], "trees": [feature_tree(ws)], "cams": [], "simulations": [], "pcbs": []}
        self.info_cache[path] = (st.st_mtime_ns, st.st_size, info)
        return info

    def summary(self, i, rel, path):
        st = os.stat(path)
        folder = os.path.dirname(path)
        proj = rel.split(os.sep)[0] if os.sep in rel else os.path.splitext(rel)[0]
        return {
            "id": doc_id(i, rel),
            "name": os.path.splitext(os.path.basename(path))[0],
            "project": proj,
            "path": path,
            "modified": int(st.st_mtime),
            "size": st.st_size,
            "git": self._git_root(folder, self.roots[i]) is not None,
            "remote": self._remote(self._git_root(folder, self.roots[i])),
            "chats": len(self.chat_list(path)),
            "busy": path in self.busy,
        }

    def _git_root(self, folder, root):
        root = os.path.realpath(root)
        d = os.path.realpath(folder)
        while d.startswith(root):
            if os.path.exists(os.path.join(d, ".git")):
                return d
            if d == root:
                break
            d = os.path.dirname(d)
        return None

    @staticmethod
    def _remote(git_root):
        """The project's GitHub repository as "owner/name" (its origin), or None."""
        if not git_root:
            return None
        try:
            with open(os.path.join(git_root, ".git", "config")) as f:
                cfg = f.read()
        except OSError:
            return None
        m = re.search(r'\[remote "origin"\][^\[]*?^\s*url\s*=\s*(\S+)', cfg, re.M)
        return short_repo(m.group(1)) if m else None

    def list_docs(self):
        docs = [self.summary(i, rel, p) for i, rel, p in self.scan()]
        docs.sort(key=lambda d: -d["modified"])
        return {"roots": self.roots, "documents": docs, "rcad": bool(self.rcad), "claude": bool(shutil.which("claude"))}

    def doc_detail(self, did):
        path = self.resolve(did)
        if not path:
            return None
        root_i = int(did.split("-", 1)[0])
        d = self.summary(root_i, os.path.relpath(path, os.path.realpath(self.roots[root_i])), path)
        d.update(self.info(path) or {})
        d["chat_list"] = self.chat_list(path)
        d["job"] = self.busy.get(path)
        return d

    def new_project(self, name):
        rcad = self.rcad
        if not rcad:
            return None, "rcad is not installed on this computer"
        clean = re.sub(r"[^A-Za-z0-9 ._-]+", "", (name or "").strip()).strip(" .")[:60]
        if not clean:
            return None, "give the project a name"
        root = self.roots[0]
        folder = os.path.join(root, clean)
        if os.path.exists(folder):
            return None, f"a project called {clean} already exists"
        os.makedirs(folder)
        doc = os.path.join(folder, clean + ".json")
        script = [{"tool": "new_document", "args": {"units": "mm"}}, {"tool": "save_document", "args": {"path": doc}}]
        err = self._run_script(rcad, script, folder, 120)
        if err or not os.path.isfile(doc):
            shutil.rmtree(folder, ignore_errors=True)
            return None, err or "rcad did not write the document"
        return doc_id(0, os.path.relpath(doc, root)), None

    # ---------------------------------------------------------------- viewer

    def _run_script(self, rcad, steps, cwd, timeout, results=None, stderr_path=None):
        """Run tool calls with `rcad run`; an error text, or None. `results` (a list) gets each step's result.
        With `stderr_path`, rcad's progress lines go to that file as they come."""
        # rCAD's kernel gives up on one Part Studio after 10 minutes unless told otherwise: let a
        # long build (a big STEP import) run as long as the whole run may
        env = dict(os.environ, RCAD_KERNEL_TIMEOUT=str(int(timeout)))
        fd, sp = tempfile.mkstemp(suffix=".json", prefix="rcad-mobile-")
        with os.fdopen(fd, "w") as f:
            json.dump(steps, f)
        try:
            # no --stop-on-error: a document whose features fail still opens and exports
            if stderr_path:
                with open(stderr_path, "w") as ef:
                    r = subprocess.run([rcad, "run", sp], cwd=cwd, stdout=subprocess.PIPE, stderr=ef, text=True, timeout=timeout, env=env)
                r.stderr = _tail(stderr_path, 2000)
            else:
                r = subprocess.run([rcad, "run", sp], cwd=cwd, capture_output=True, text=True, timeout=timeout, env=env)
        except subprocess.TimeoutExpired:
            return "rcad took too long"
        except OSError as e:
            return f"could not run rcad: {e}"
        finally:
            os.unlink(sp)
        out = []
        for ln in r.stdout.splitlines():
            try:
                v = json.loads(ln)
            except ValueError:
                continue
            if isinstance(v, dict) and "tool" in v:
                out.append(v)
        if results is not None:
            results.extend(out)
        if r.returncode != 0:
            # the first step that failed outright (not one that only reports failing features)
            hard = [v["error"] for v in out if isinstance(v.get("error"), str)]
            if hard:
                return hard[0]
            if not out:
                return r.stderr.strip()[-400:] or f"rcad failed ({r.returncode})"
        return None

    def _page_key(self, path, kind, index, page):
        """The cache key and page path for one tab of a document as it is now."""
        st = os.stat(path)
        sel = f"{kind}{index}{'-' + page if page else ''}"
        try:  # a new rcad build can change the page (it is compiled in), so it is part of the key
            built = os.stat(self.rcad).st_mtime_ns
        except OSError:
            built = 0
        key = hashlib.sha256(f"{path}\0{st.st_mtime_ns}\0{st.st_size}\0{sel}\0{VIEW_VERSION}\0{built}".encode()).hexdigest()[:24]
        return key, os.path.join(CACHE_DIR, f"{key}.html"), sel

    def _check(self, did, kind, index, page):
        """(path, index, page, error) for a tab request."""
        path = self.resolve(did)
        if not path:
            return None, 0, "", "no such document"
        if kind not in TAB_KINDS:
            return None, 0, "", f"can't show a {kind} tab"
        if not self.rcad:
            return None, 0, "", "rcad is not installed on this computer"
        index = max(0, index or 0)
        have = (self.info(path) or {}).get(TAB_KINDS[kind])
        if isinstance(have, list) and index >= len(have) and not (kind == "studio" and index == 0):
            return None, 0, "", "no such tab"
        return path, index, re.sub(r"[^a-z0-9]", "", page or "")[:16], None

    def view(self, did, kind="studio", index=0, page=""):
        """Path of the page showing one tab of a document (made with rCAD when the document changed),
        or (None, error). The same run writes what the phone lists beside it (see `tree`). For a
        kind in BACKGROUND_KINDS the error is "building" while rCAD works on it (see `progress`)."""
        path, index, page, err = self._check(did, kind, index, page)
        if err:
            return None, err
        os.makedirs(CACHE_DIR, exist_ok=True)
        # a Simulation tab's pages (3D and pictures) come from one solve, so they share one build
        shared = kind in SHARED_BUILD_KINDS
        key, out, sel = self._page_key(path, kind, index, "" if shared else page)
        if os.path.isfile(out) and os.path.isfile(out[:-5] + ".tree.json"):
            return self._companion(out, page) if shared else out, None
        if kind in BACKGROUND_KINDS:
            with self.lock:
                err = self.build_errors.pop(key, None)
                if err:
                    return None, err  # shown once; asking again tries again
                if key not in self.building:
                    self.building[key] = time.time()
                    self.build_done[key] = threading.Event()
                    threading.Thread(target=self._build_bg, args=(path, kind, index, "" if shared else page, key, out, sel), daemon=True).start()
                done = self.build_done.get(key)
            # most pages build in a second or two: wait for those rather than show the progress page
            if done and done.wait(max(0.0, QUICK_WAIT - (time.time() - self.building.get(key, time.time())))):
                with self.lock:
                    err = self.build_errors.pop(key, None)
                if err:
                    return None, err
                if os.path.isfile(out) and os.path.isfile(out[:-5] + ".tree.json"):
                    return self._companion(out, page) if shared else out, None
            return None, "building"
        return self._build(path, kind, index, page, key, out, sel)

    def gzipped(self, out):
        """A gzip copy of a cached page beside it (made once; a big assembly's page halves), or None
        when it isn't worth it or can't be made."""
        gz = out + ".gz"
        try:
            if os.path.getsize(out) < GZIP_MIN:
                return None
            if os.path.isfile(gz) and os.path.getmtime(gz) >= os.path.getmtime(out):
                return gz
        except OSError:
            return None
        with self.lock:
            lk = self.export_locks.setdefault(gz, threading.Lock())
        with lk:
            if not (os.path.isfile(gz) and os.path.getmtime(gz) >= os.path.getmtime(out)):
                tmp = gz + ".tmp"
                try:
                    with open(out, "rb") as src, gzip.open(tmp, "wb", compresslevel=6) as dst:
                        shutil.copyfileobj(src, dst, 1 << 20)
                    os.replace(tmp, gz)
                except OSError:
                    return None
        return gz

    @staticmethod
    def _companion(out, page):
        """The page `page` of a shared build: written beside the main one as <key>.<page>.html."""
        if not page:
            return out
        other = out[:-5] + f".{page}.html"
        return other if os.path.isfile(other) else out

    def _build_bg(self, path, kind, index, page, key, out, sel):
        try:
            _, err = self._build(path, kind, index, page, key, out, sel, stderr_path=out[:-5] + ".progress", timeout=BACKGROUND_TIMEOUT)
        except Exception as e:  # keep the receiver up whatever happens
            err = f"{type(e).__name__}: {e}"
        with self.lock:
            self.building.pop(key, None)
            if err:
                self.build_errors[key] = err
            done = self.build_done.pop(key, None)
        if done:
            done.set()

    def progress(self, did, kind="studio", index=0, page=""):
        """For a page being built in the background: (seconds so far, rCAD's last progress line)."""
        path, index, page, err = self._check(did, kind, index, page)
        if err:
            return 0, ""
        key, out, _ = self._page_key(path, kind, index, "" if kind in SHARED_BUILD_KINDS else page)
        with self.lock:
            t0 = self.building.get(key)
        lines = [ln.strip() for ln in re.split(r"[\r\n]+", _tail(out[:-5] + ".progress", 600)) if ln.strip()]
        if kind != "simulation":
            # rCAD's own progress lines, not the modelling kernel's messages
            lines = [ln for ln in lines if ln.startswith("Building ")]
        return (time.time() - t0 if t0 else 0), (lines[-1] if lines else "")

    def _build(self, path, kind, index, page, key, out, sel, stderr_path=None, timeout=EXPORT_TIMEOUT):
        with self.lock:
            lk = self.export_locks.setdefault(key, threading.Lock())
        with lk:
            if os.path.isfile(out) and os.path.isfile(out[:-5] + ".tree.json"):
                return out, None
            try:
                with open(path) as f:
                    ws = json.load(f)
            except (OSError, ValueError):
                ws = {}
            tmp = out + ".tmp.html"
            steps, finish = page_steps(kind, index, page, tmp, ws if isinstance(ws, dict) else {})
            steps.insert(0, {"tool": "open_document", "args": {"path": path}})
            results = []
            err = self._run_script(self.rcad, steps, os.path.dirname(path), timeout, results, stderr_path)
            by = {}
            for v in results:
                if isinstance(v.get("result"), dict):
                    by[v["tool"]] = v["result"]
            tree = finish(by, tmp)
            if not os.path.isfile(tmp):
                return None, err or "rcad wrote no page"
            with open(out[:-5] + ".tree.json", "w") as f:
                json.dump(tree, f)
            for n in os.listdir(os.path.dirname(tmp)):
                # other pages the same run wrote: <tmp>.<page>.html
                pre = os.path.basename(tmp) + "."
                if n.startswith(pre) and n.endswith(".html"):
                    os.replace(os.path.join(os.path.dirname(tmp), n), out[:-5] + "." + n[len(pre):])
            os.replace(tmp, out)
            self._prune_cache(path, sel, out)
        return out, None

    def tree(self, did, kind="studio", index=0, page=""):
        """What the phone lists beside the page: see `view`."""
        if kind in SHARED_BUILD_KINDS:
            page = ""  # one tree for all the pages of the build
        out, err = self.view(did, kind, index, page)
        if err == "building":
            secs, line = self.progress(did, kind, index, page)
            return {"kind": kind, "building": True, "seconds": int(secs), "progress": line}, None
        if not out:
            return None, err
        try:
            with open(out[:-5] + ".tree.json") as f:
                return json.load(f), None
        except (OSError, ValueError) as e:
            return None, str(e)

    def _prune_cache(self, path, sel, keep):
        """Keep the cache small: pages older than a week go."""
        cutoff = time.time() - 7 * 86400
        for n in os.listdir(CACHE_DIR):
            p = os.path.join(CACHE_DIR, n)
            try:
                if p != keep and os.path.getmtime(p) < cutoff:
                    os.unlink(p)
            except OSError:
                pass

    # ---------------------------------------------------------------- chats

    def chat_list(self, path):
        d = os.path.join(os.path.dirname(path), ".rcad-chats")
        name = os.path.basename(path)
        out = []
        try:
            names = os.listdir(d)
        except OSError:
            return out
        for n in names:
            if not n.endswith(".json") or n.endswith(".context.json") or n.startswith("."):
                continue
            try:
                with open(os.path.join(d, n)) as f:
                    m = json.load(f)
            except (OSError, ValueError):
                continue
            if m.get("document") != name:
                continue
            lines = m.get("lines") or []
            out.append({"id": m.get("id"), "title": m.get("title"), "updated": m.get("updated"), "created": m.get("created"),
                        "model": m.get("model"), "requests": sum(1 for ln in lines if ln.get("kind") == "user")})
        out.sort(key=lambda c: -(c.get("updated") or 0))
        return out

    def chat(self, did, cid):
        path = self.resolve(did)
        if not path or not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", cid or ""):
            return None
        p = os.path.join(os.path.dirname(path), ".rcad-chats", cid + ".json")
        try:
            with open(p) as f:
                m = json.load(f)
        except (OSError, ValueError):
            return None
        return m if m.get("document") == os.path.basename(path) else None

    def start_chat(self, did, body):
        path = self.resolve(did)
        if not path:
            return 404, {"error": "no such document"}
        rcad = self.rcad
        if not rcad:
            return 503, {"error": "rcad is not installed on this computer"}
        text = str(body.get("text") or "").strip()
        if not text:
            return 400, {"error": "say what to do"}
        cid = body.get("chat_id")
        if cid is not None and not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", str(cid)):
            return 400, {"error": "bad chat id"}
        with self.lock:
            running = self.busy.get(path)
            if running and running in self.jobs and not self.jobs[running].done:
                return 409, {"error": "Claude is still working on this document", "job": running}
            fd, pf = tempfile.mkstemp(suffix=".txt", prefix="rcad-prompt-")
            with os.fdopen(fd, "w") as f:
                f.write(text)
            cmd = [rcad, "chat", "--doc", path, "--prompt-file", pf]
            if cid:
                cmd += ["--chat", str(cid)]
            if isinstance(body.get("studio"), int) and body["studio"] >= 0:
                cmd += ["--studio", str(body["studio"])]
            model = MODELS.get(str(body.get("model") or ""), self.model)
            if model:
                cmd += ["--model", model]
            if self.auth:
                cmd += ["--auth", self.auth]
            if body.get("effort") in EFFORTS:
                cmd += ["--effort", body["effort"]]
            try:
                proc = subprocess.Popen(cmd, cwd=os.path.dirname(path), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, bufsize=1)
            except OSError as e:
                os.unlink(pf)
                return 500, {"error": f"could not start rcad: {e}"}
            jid = secrets.token_hex(8)
            job = Job(jid, path, proc, cid, text)
            self.jobs[jid] = job
            self.busy[path] = jid
            self._gc()
        threading.Thread(target=self._pump, args=(job, pf), daemon=True).start()
        # the chat id is the first line rcad prints: wait for it briefly so the phone knows it
        evs, _, _ = job.since(0, 5)
        return 200, {"job": jid, "chat_id": job.chat_id}

    def _pump(self, job, prompt_file):
        err_lines = []
        t = threading.Thread(target=lambda: err_lines.extend(job.proc.stderr.read().splitlines()[-20:]), daemon=True)
        t.start()
        try:
            for line in job.proc.stdout:
                line = line.strip()
                if not line.startswith("{"):
                    continue
                try:
                    ev = json.loads(line)
                except ValueError:
                    continue
                if isinstance(ev, dict):
                    job.add(ev)
            code = job.proc.wait()
            t.join(2)
            error = None
            if code != 0 and not any(e.get("type") == "done" for e in job.events):
                tail = "\n".join(err_lines[-4:]).strip()
                error = f"rcad chat stopped ({code})" + (f": {tail}" if tail else "")
            job.finish(error)
        finally:
            try:
                os.unlink(prompt_file)
            except OSError:
                pass
            with self.lock:
                if self.busy.get(job.doc) == job.id:
                    del self.busy[job.doc]

    def stop(self, jid):
        job = self.jobs.get(jid)
        if not job:
            return False
        if not job.done:
            try:
                job.proc.stdin.write("stop\n")
                job.proc.stdin.flush()
            except (OSError, ValueError):
                pass
            # Claude Code may take a moment to wind down; make sure it ends
            threading.Timer(30, lambda: job.done or job.proc.poll() is not None or job.proc.terminate()).start()
        return True

    # ---------------------------------------------------------------- GitHub

    def github(self, args, timeout=90):
        """Run `rcad github <args>`: (HTTP status, its JSON)."""
        rcad = self.rcad
        if not rcad:
            return 503, {"error": "rcad is not installed on this computer"}
        try:
            r = subprocess.run([rcad, "github", *args], capture_output=True, text=True, timeout=timeout,
                               stdin=subprocess.DEVNULL, cwd=self.roots[0] if os.path.isdir(self.roots[0]) else None)
        except subprocess.TimeoutExpired:
            return 504, {"error": "GitHub took too long to answer"}
        except OSError as e:
            return 500, {"error": f"could not run rcad: {e}"}
        line = next((x for x in reversed(r.stdout.splitlines()) if x.startswith("{")), "")
        try:
            v = json.loads(line)
        except ValueError:
            tail = (r.stderr.strip().splitlines() or [f"rcad github stopped ({r.returncode})"])[-1]
            return 500, {"error": tail}
        if r.returncode != 0 and "error" not in v:
            v["error"] = f"rcad github stopped ({r.returncode})"
        return (200 if r.returncode == 0 else 400), v

    def github_status(self, fresh=False):
        with self.lock:
            hit = self.gh_status
        if hit and not fresh and time.time() - hit[0] < 60:
            return 200, hit[1]
        code, v = self.github(["status"], 30)
        if code == 200:
            with self.lock:
                self.gh_status = (time.time(), v)
        return code, v

    def github_login(self):
        code, v = self.github(["login-start"], 30)
        if code == 200:
            with self.lock:
                self.gh_device = (v.pop("device_code", ""), time.time() + float(v.get("expires_in") or 900))
        return code, v

    def github_poll(self):
        with self.lock:
            dev = self.gh_device
        if not dev or time.time() > dev[1]:
            return 400, {"error": "the code expired: start again"}
        code, v = self.github(["login-poll", "--code", dev[0]], 30)
        if code == 200 and v.get("state") == "done" or code != 200:
            with self.lock:
                self.gh_device = None
                self.gh_status = None
        return code, v

    def github_logout(self):
        code, v = self.github(["logout"], 30)
        with self.lock:
            self.gh_status = None
        return code, v

    def github_repos(self):
        code, v = self.github(["repos"], 90)
        if code != 200:
            return code, v
        here = {}  # "owner/name" (lower case) -> document id of a copy on netdash
        for i, rel, path in self.scan():
            r = self._remote(self._git_root(os.path.dirname(path), self.roots[i]))
            if r:
                here.setdefault(r.lower(), doc_id(i, rel))
        for r in v.get("repos") or []:
            d = here.get(str(r.get("full_name", "")).lower())
            if d:
                r["doc"] = d
        return code, v

    def github_clone(self, url):
        url = str(url or "").strip()
        if not re.fullmatch(r"https://[A-Za-z0-9.-]+/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+?(\.git)?/?", url):
            return 400, {"error": "give the repository's https address"}
        root = self.roots[0]
        os.makedirs(root, exist_ok=True)
        code, v = self.github(["clone", "--url", url, "--into", root], 300)
        if code != 200:
            return code, v
        doc = v.get("doc") or ""
        rroot = os.path.realpath(root)
        if not os.path.realpath(doc).startswith(rroot + os.sep):
            return 500, {"error": "rcad cloned it somewhere else"}
        if v.get("empty"):
            # an empty repository: start the project with an empty document, committed and pushed
            err = self._run_script(self.rcad, [{"tool": "new_document", "args": {"units": "mm"}},
                                               {"tool": "save_document", "args": {"path": doc}}], os.path.dirname(doc), 120)
            if err or not os.path.isfile(doc):
                return 500, {"error": err or "rcad did not write the document"}
            self.github(["sync", "--doc", doc, "--message", "Started from the rCAD phone app"], 120)
        return 200, {"id": doc_id(0, os.path.relpath(os.path.realpath(doc), rroot)), "empty": bool(v.get("empty")),
                     "synced": v.get("synced")}

    def doc_git(self, did, args):
        """sync / resolve a document's project: (HTTP status, JSON)."""
        path = self.resolve(did)
        if not path:
            return 404, {"error": "no such document"}
        with self.lock:
            running = self.busy.get(path)
            if running and running in self.jobs and not self.jobs[running].done:
                return 409, {"error": "Claude is still working on this document"}
        return self.github([args[0], "--doc", path, *args[1:]], 180)

    def _gc(self):
        now = time.time()
        for jid in [j for j, job in self.jobs.items() if job.done and now - (job.finished_at or now) > JOB_KEEP_SECS]:
            del self.jobs[jid]


def short_repo(url):
    """ "owner/name" of a GitHub remote URL (https or ssh), else None."""
    m = re.match(r"(?:https?://(?:[^@/]+@)?github\.com/|git@github\.com:|ssh://git@github\.com/)([^/]+)/([^/]+?)(?:\.git)?/?$", url.strip())
    return f"{m.group(1)}/{m.group(2)}" if m else None


def _param_text(params):
    if not isinstance(params, dict):
        return ""
    bits = []
    for k in SHOWN_PARAMS:
        v = params.get(k)
        if v is None or v == "" or isinstance(v, (dict, list)):
            continue
        if k == "end" and v == "blind":
            continue
        if k == "op" and v in ("add", "new"):
            continue
        bits.append(f"{k} {v}")
    return ", ".join(bits[:3])


def _names(items, word):
    return [x.get("name") or f"{word} {n + 1}" if isinstance(x, dict) else f"{word} {n + 1}" for n, x in enumerate(items or [])]


def _mins(secs):
    secs = int(round(secs or 0))
    return f"{secs // 60} min {secs % 60:02d} s" if secs >= 60 else f"{secs} s"


def _tail(path, n):
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - n))
            return f.read().decode("utf-8", "replace")
    except OSError:
        return ""


def page_steps(kind, index, page, out, ws=None):
    """The rcad run steps that write the page for one tab to `out`, and a function that turns their
    results (by tool name) into what the phone lists beside it."""
    if kind == "assembly":
        return ([{"tool": "assembly_info", "args": {"assembly": index}},
                 {"tool": "export_web", "args": {"path": out, "assembly": index, "max_triangles": PHONE_TRIANGLES}}],
                lambda by, _out: assembly_tree(by.get("assembly_info") or {}))
    if kind == "cam":
        export = ({"tool": "export_setup_sheet", "args": {"cam": index, "path": out}} if page == "sheet"
                  else {"tool": "export_cam_simulation", "args": {"cam": index, "path": out}})
        return ([{"tool": "cam_info", "args": {"cam": index}}, {"tool": "simulate_cam", "args": {"cam": index}}, export],
                lambda by, _out: cam_tree(by.get("cam_info") or {}, by.get("simulate_cam") or {}))
    if kind == "simulation":
        return sim_steps(index, out, ws or {})
    if kind == "pcb":
        return pcb_steps(index, page, out, ws or {})
    if kind == "drawing":
        sheet = int(page) if page.isdigit() and int(page) > 0 else 1
        svg = out + ".svg"

        def finish(by, out):
            info = by.get("drawing_info") or {}
            if os.path.isfile(svg):
                with open(svg) as f:
                    body = f.read()
                os.remove(svg)
                title = f"{info.get('name') or 'Drawing'} · sheet {sheet}"
                with open(out, "w") as f:
                    f.write(SHEET_PAGE.replace("__TITLE__", html_escape(title)).replace("__SVG__", body))
            return drawing_tree(info, sheet)
        return ([{"tool": "drawing_info", "args": {"drawing": index}},
                 {"tool": "export_drawing", "args": {"drawing": index, "path": svg, "format": "svg", "sheet": sheet}}], finish)
    steps = [{"tool": "switch_part_studio", "args": {"index": index}}] if index else []
    steps += [{"tool": "describe", "args": {}}, {"tool": "export_web", "args": {"path": out, "studio": index, "max_triangles": PHONE_TRIANGLES}}]
    return steps, lambda by, _out: studio_tree(by.get("describe") or by.get("open_document") or {})


def assembly_tree(a):
    return {
        "kind": "assembly",
        "name": a.get("name"),
        "instances": [{"id": i.get("id"), "name": i.get("name"), "fixed": bool(i.get("fixed")), "hidden": bool(i.get("hidden")),
                       "sub": i.get("sub_assembly") is not None} for i in a.get("instances") or []],
        "mates": [{"name": m.get("name") or m.get("kind"), "kind": m.get("kind"), "between": m.get("between") or m.get("instances")}
                  for m in a.get("mates") or []],
        "failed_mates": a.get("failed_mates") or [],
        "bom": a.get("bom") or [],
    }


def studio_tree(d):
    errors = {e.get("feature"): e.get("error") for e in d.get("errors") or [] if isinstance(e, dict)}
    feats = []
    for f in d.get("features") or []:
        item = {"id": f.get("id"), "name": f.get("name"), "kind": f.get("kind"), "suppressed": bool(f.get("suppressed")),
                "detail": _param_text(f.get("params"))}
        if f.get("id") in errors:
            item["error"] = errors[f.get("id")]
        feats.append(item)
    return {
        "kind": "studio",
        "units": d.get("units"),
        "features": feats,
        "variables": d.get("variables") or [],
        "parts": [{"name": p.get("name"), "volume_mm3": p.get("volume_mm3"), "bbox_mm": p.get("bbox_mm")} for p in d.get("parts") or []],
        "errors": [e for e in d.get("errors") or []],
        "warnings": len(d.get("warnings") or []),
    }


def html_escape(t):
    return t.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


# Pinch or wheel to zoom, drag to pan, double-tap to switch between fitted and 3x, for #sheet
# (a sheet or a picture); the page calls setSize(width, height) with its natural size.
ZOOM_JS = """
const el = document.getElementById('sheet');
let W = 1000, H = 700, s = 1, x = 0, y = 0, fitS = 1;
const TOP = (document.getElementById('bar') || { offsetHeight: 0 }).offsetHeight;
function apply() { el.style.transform = `translate(${x}px, ${y}px) scale(${s})`; }
function fit() {
  const m = 12, vw = innerWidth - 2 * m, vh = innerHeight - TOP - 2 * m;
  fitS = Math.min(vw / W, vh / H); s = fitS; x = (innerWidth - W * s) / 2; y = TOP + (innerHeight - TOP - H * s) / 2; apply();
}
function setSize(w, h) { W = w; H = h; el.style.width = W + 'px'; el.style.height = H + 'px'; fit(); }
function zoomAt(px, py, k) {
  const ns = Math.min(Math.max(s * k, fitS * 0.5), fitS * 20);
  x = px - (px - x) * ns / s; y = py - (py - y) * ns / s; s = ns; apply();
}
const pts = new Map(); let last = null, tapAt = 0, moved = false;
addEventListener('pointerdown', e => { if (e.target.closest('button')) return; pts.set(e.pointerId, [e.clientX, e.clientY]); last = null; moved = false; });
addEventListener('pointermove', e => {
  if (!pts.has(e.pointerId)) return;
  pts.set(e.pointerId, [e.clientX, e.clientY]);
  const p = [...pts.values()];
  const cx = p.reduce((a, q) => a + q[0], 0) / p.length, cy = p.reduce((a, q) => a + q[1], 0) / p.length;
  const d = p.length > 1 ? Math.hypot(p[0][0] - p[1][0], p[0][1] - p[1][1]) : 0;
  if (last && last.n === p.length) {
    x += cx - last.cx; y += cy - last.cy;
    if (d && last.d) zoomAt(cx, cy, d / last.d); else apply();
    if (Math.abs(cx - last.cx) + Math.abs(cy - last.cy) > 2) moved = true;
  }
  last = { n: p.length, cx, cy, d };
});
function up(e) {
  if (!pts.has(e.pointerId)) return;
  pts.delete(e.pointerId); last = null;
  if (e.type === 'pointerup' && !moved && pts.size === 0) {
    const now = Date.now();
    if (now - tapAt < 300) { if (s > fitS * 1.5) fit(); else zoomAt(e.clientX, e.clientY, 3 * fitS / s); tapAt = 0; } else tapAt = now;
  }
}
addEventListener('pointerup', up); addEventListener('pointercancel', up);
addEventListener('wheel', e => { e.preventDefault(); zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY / 300)); }, { passive: false });
addEventListener('resize', fit);
"""

# A drawing sheet on the phone: the SVG fitted to the screen, zoomable.
SHEET_PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
<link rel="icon" href="data:,"><title>__TITLE__</title>
<style>
html, body { margin: 0; height: 100%; overflow: hidden; background: #e8e9ec; touch-action: none; }
#sheet { position: absolute; left: 0; top: 0; transform-origin: 0 0; box-shadow: 0 1px 6px rgba(0,0,0,.25); background: #fff; }
#sheet svg { display: block; width: 100%; height: 100%; }
#hint { position: fixed; right: 10px; top: 10px; font: 12px system-ui, sans-serif; color: #555; background: rgba(255,255,255,.85);
  border-radius: 6px; padding: 4px 8px; pointer-events: none; }
</style></head><body>
<div id="sheet">__SVG__</div><div id="hint">pinch to zoom · double-tap</div>
<script>
""" + ZOOM_JS + """
const vb = (el.querySelector('svg').getAttribute('viewBox') || '0 0 420 297').split(/[ ,]+/).map(Number);
setSize(1000, 1000 * vb[3] / vb[2]);
</script></body></html>
"""

# FEA results on the phone: rCAD's colour-map pictures (each with its own scale and caption),
# a button per picture, zoomable. __PICS__ is a JSON list of {label, src}.
SIM_PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
<link rel="icon" href="data:,"><title>__TITLE__</title>
<style>
html, body { margin: 0; height: 100%; overflow: hidden; background: #202328; touch-action: none; font: 13px system-ui, sans-serif; }
#bar { position: fixed; left: 0; right: 0; top: 0; z-index: 2; display: flex; gap: 6px; padding: 8px 10px; overflow-x: auto;
  background: #1a1c20; scrollbar-width: none; }
#bar button { flex: none; font: inherit; color: #c9ced6; background: #2a2e35; border: 1px solid #3a3f47; border-radius: 16px; padding: 5px 12px; }
#bar button.on { color: #fff; background: #2F80ED; border-color: #2F80ED; }
#sheet { position: absolute; left: 0; top: 0; transform-origin: 0 0; }
#sheet img, #sheet svg { display: block; width: 100%; height: 100%; }
</style></head><body>
<div id="bar"></div><div id="sheet"></div>
<script>
const PICS = __PICS__;
""" + ZOOM_JS + """
const bar = document.getElementById('bar');
function show(i) {
  [...bar.children].forEach((b, k) => b.classList.toggle('on', k === i));
  const p = PICS[i];
  if (p.svg) { el.innerHTML = p.svg; setSize(p.w, p.h); return; }
  const img = new Image();
  img.onload = () => { el.innerHTML = ''; el.appendChild(img); setSize(img.naturalWidth, img.naturalHeight); };
  img.src = p.src;
}
PICS.forEach((p, i) => { const b = document.createElement('button'); b.textContent = p.label; b.onclick = () => show(i); bar.appendChild(b); });
if (PICS.length < 2) bar.style.display = 'none';
show(0);
</script></body></html>
"""

# Shown while a page is built in the background: reloads itself until the result is there.
BUILDING_PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="data:,"><title>Working…</title>
<style>
html, body { margin: 0; height: 100%; background: #e8e9ec; color: #444; font: 15px system-ui, sans-serif; }
body { display: grid; place-items: center; text-align: center; padding: 0 24px; box-sizing: border-box; }
.t { font-size: 13px; color: #777; margin-top: 6px; }
.bar { width: 180px; height: 4px; margin: 14px auto 0; border-radius: 2px; background: #d3d6db; overflow: hidden; }
.bar div { width: 40%; height: 100%; background: #2F80ED; animation: go 1.4s ease-in-out infinite; }
@keyframes go { from { transform: translateX(-100%); } to { transform: translateX(250%); } }
</style></head><body>
<div><div>__WHAT__</div><div class="t">__PROGRESS__</div><div class="bar"><div></div></div></div>
<script>
// the rCAD phone app: this is netdash still working, not the page loading
if (window.RcadApp && RcadApp.building) RcadApp.building();
setTimeout(() => location.reload(), 3000);
</script>
</body></html>
"""


def building_page(kind, secs, line):
    what = {"simulation": "Solving the simulation on netdash…", "assembly": "Building the assembly on netdash…",
            "studio": "Building the Part Studio on netdash…"}.get(kind, "Working on netdash…")
    if kind in ("assembly", "studio") and secs > 20:
        what += "<div class=t>The first time takes longest: every Part Studio it uses is built and kept for next time.</div>"
    m, sec = divmod(int(secs), 60)
    prog = (html_escape(line) + " · " if line else "") + f"{m}:{sec:02d}"
    return BUILDING_PAGE.replace("__WHAT__", what).replace("__PROGRESS__", prog)


def _png_size(path):
    try:
        with open(path, "rb") as f:
            head = f.read(24)
        return int.from_bytes(head[16:20], "big"), int.from_bytes(head[20:24], "big")
    except OSError:
        return 0, 0


def sim_steps(index, out, ws):
    """Solve a Simulation tab and render its results: static stress, displacement and factor of
    safety; modal each mode (up to 6); transient stress and displacement plus the response over time."""
    sims = ws.get("simulations") or []
    study = sims[index] if index < len(sims) and isinstance(sims[index], dict) else {}
    analysis = study.get("analysis") or "static"
    if analysis == "modal":
        n = study.get("modes") if isinstance(study.get("modes"), int) else 6
        pics = [(f"mode{k}", {"mode": k}) for k in range(1, max(1, min(n, 6)) + 1)]
    elif analysis == "transient":
        pics = [("Stress", {}), ("Displacement", {"field": "displacement"})]
    else:
        pics = [("Stress", {}), ("Displacement", {"field": "displacement"}), ("Safety", {"field": "safety"})]
    steps = [{"tool": "solve_simulation", "args": {"simulation": index}},
             {"tool": "export_simulation_web", "args": {"simulation": index, "path": out}}]
    files = []
    for k, (label, extra) in enumerate(pics):
        f = f"{out}.{k}.png"
        files.append((label, f))
        steps.append({"tool": "render_simulation", "args": dict({"simulation": index, "path": f, "height": 900}, **extra)})

    def finish(by, out):
        res = by.get("solve_simulation") or {}
        modes = {m.get("mode"): m for m in res.get("modes") or [] if isinstance(m, dict)}
        named = []
        for label, f in files:
            if label.startswith("mode"):
                k = int(label[4:])
                hz = (modes.get(k) or {}).get("frequency_hz")
                label = f"Mode {k}" + (f" · {_hz(hz)}" if hz else "")
            named.append((label, f))
        hist = res.get("history")
        chart = history_chart(hist) if analysis == "transient" and isinstance(hist, list) and len(hist) > 1 else None
        write_pictures(out + ".pictures.html", f"{study.get('name') or 'Simulation'} · {analysis}", pictures(named, [chart]))
        if not os.path.isfile(out) and os.path.isfile(out + ".pictures.html"):
            # an rcad without export_simulation_web: the pictures are the page
            shutil.copyfile(out + ".pictures.html", out)
        return sim_tree(study, res)
    return steps, finish


def _hz(f):
    return f"{f / 1000:.3g} kHz" if f >= 1000 else f"{f:.3g} Hz"


def history_chart(hist):
    """The transient response (displacement over time) as an SVG picture for the results page."""
    w, h, l, r, t, b = 900, 560, 70, 20, 40, 60
    ts = [p[0] for p in hist if isinstance(p, list) and len(p) == 2]
    ys = [p[1] for p in hist if isinstance(p, list) and len(p) == 2]
    t1, y1 = (max(ts) or 1), (max(ys) or 1)
    sx = lambda v: l + (w - l - r) * v / t1
    sy = lambda v: h - b - (h - t - b) * v / y1
    pts = " ".join(f"{sx(a):.1f},{sy(c):.1f}" for a, c in zip(ts, ys))
    unit, k = ("ms", 1e3) if t1 < 1 else ("s", 1)
    grid = ""
    for i in range(5):
        gv = y1 * i / 4
        grid += (f'<line x1="{l}" x2="{w - r}" y1="{sy(gv):.1f}" y2="{sy(gv):.1f}" stroke="#3a3f47"/>'
                 f'<text x="{l - 8}" y="{sy(gv) + 4:.1f}" text-anchor="end">{gv:.3g}</text>')
        tv = t1 * i / 4
        grid += f'<text x="{sx(tv):.1f}" y="{h - b + 20}" text-anchor="middle">{tv * k:.3g}</text>'
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" font-family="system-ui,sans-serif" font-size="14" fill="#c9ced6">'
           f'<rect width="{w}" height="{h}" fill="#202328"/><text x="{l}" y="24" font-size="16">Peak displacement over time (mm)</text>'
           f'{grid}<polyline points="{pts}" fill="none" stroke="#2F80ED" stroke-width="2.5"/>'
           f'<text x="{(w + l) / 2}" y="{h - 14}" text-anchor="middle">time ({unit})</text></svg>')
    return {"label": "Over time", "svg": svg, "w": w, "h": h}


def pictures(files, extra=()):
    """[{label, src}] for the pictures page from (label, png path) pairs that exist (each removed)."""
    shown = []
    for label, f in files:
        if os.path.isfile(f):
            with open(f, "rb") as fh:
                shown.append({"label": label, "src": "data:image/png;base64," + base64.b64encode(fh.read()).decode()})
            os.remove(f)
    return shown + [e for e in extra if e]


def write_pictures(out, title, shown):
    if shown:
        with open(out, "w") as fh:
            fh.write(SIM_PAGE.replace("__TITLE__", html_escape(title)).replace("__PICS__", json.dumps(shown).replace("</", "<\\/")))


def pcb_steps(index, page, out, ws):
    """A PCB tab: the board (top and bottom copper), the schematic and, when the board has a circuit
    simulation set up, its probes' waveforms; page "3d" is the board with its parts in 3D (pcb_3d,
    not saved). The tray lists unrouted connections, probe results, parts and nets."""
    pcbs = ws.get("pcbs") or []
    pcb = pcbs[index] if index < len(pcbs) and isinstance(pcbs[index], dict) else {}
    sim = pcb.get("sim") if isinstance(pcb.get("sim"), dict) else {}
    steps = [{"tool": "pcb_info", "args": {"pcb": index}}]
    csv = out + ".csv"
    if sim.get("probes"):
        steps.append({"tool": "simulate_pcb", "args": {"pcb": index, "csv": csv}})
    files = []
    if page == "3d":
        steps += [{"tool": "pcb_3d", "args": {"pcb": index}}, {"tool": "export_web", "args": {"path": out, "max_triangles": PHONE_TRIANGLES}}]
    else:
        for k, (label, extra) in enumerate([("Top", {}), ("Bottom", {"layer": "bottom"}), ("Schematic", {"view": "schematic"})]):
            f = f"{out}.{k}.png"
            files.append((label, f))
            steps.append({"tool": "render_pcb", "args": dict({"pcb": index, "path": f, "width": 1400}, **extra)})

    def finish(by, out):
        info = by.get("pcb_info") or {}
        res = by.get("simulate_pcb") or {}
        chart = waves_chart(csv) if os.path.isfile(csv) else None
        if os.path.isfile(csv):
            os.remove(csv)
        if page != "3d":
            write_pictures(out, f"{info.get('name') or 'PCB'}", pictures(files, [chart]))
        return pcb_tree(info, res)
    return steps, finish


def waves_chart(csv):
    """The circuit simulation's probes over time, one strip each, from simulate_pcb's CSV."""
    try:
        with open(csv) as f:
            head = f.readline().strip().split(",")
            rows = [[float(x) for x in ln.split(",")] for ln in f if ln.strip()]
    except (OSError, ValueError):
        return None
    if len(head) < 2 or len(rows) < 2:
        return None
    n = len(head) - 1
    w, strip, l, r, t = 900, 170, 80, 20, 20
    h = t + n * strip + 40
    t1 = rows[-1][0] or 1
    unit, k = ("ms", 1e3) if t1 < 1 else ("s", 1)
    sx = lambda v: l + (w - l - r) * v / t1
    colours = ["#2F80ED", "#F2994A", "#27AE60", "#EB5757", "#9B51E0", "#56CCF2"]
    body = ""
    for i in range(n):
        ys = [row[i + 1] for row in rows]
        lo, hi = min(ys), max(ys)
        if hi - lo < 1e-12:
            hi = lo + 1
        top = t + i * strip
        sy = lambda v, top=top, lo=lo, hi=hi: top + 22 + (strip - 44) * (1 - (v - lo) / (hi - lo))
        pts = " ".join(f"{sx(row[0]):.1f},{sy(row[i + 1]):.1f}" for row in rows)
        body += (f'<text x="{l}" y="{top + 14}" font-size="15">{html_escape(head[i + 1])}</text>'
                 f'<line x1="{l}" x2="{w - r}" y1="{sy(lo):.1f}" y2="{sy(lo):.1f}" stroke="#3a3f47"/>'
                 f'<line x1="{l}" x2="{w - r}" y1="{sy(hi):.1f}" y2="{sy(hi):.1f}" stroke="#3a3f47"/>'
                 f'<text x="{l - 8}" y="{sy(hi) + 4:.1f}" text-anchor="end">{hi:.3g}</text>'
                 f'<text x="{l - 8}" y="{sy(lo) + 4:.1f}" text-anchor="end">{lo:.3g}</text>'
                 f'<polyline points="{pts}" fill="none" stroke="{colours[i % len(colours)]}" stroke-width="2"/>')
    for j in range(5):
        tv = t1 * j / 4
        body += f'<text x="{sx(tv):.1f}" y="{h - 22}" text-anchor="middle">{tv * k:.3g}</text>'
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" font-family="system-ui,sans-serif" font-size="13" fill="#c9ced6">'
           f'<rect width="{w}" height="{h}" fill="#202328"/>{body}<text x="{(w + l) / 2}" y="{h - 4}" text-anchor="middle">time ({unit})</text></svg>')
    return {"label": "Circuit", "svg": svg, "w": w, "h": h}


def _eng(v, unit):
    if not isinstance(v, (int, float)):
        return "?"
    a = abs(v)
    for f, p in ((1e6, "M"), (1e3, "k"), (1, ""), (1e-3, "m"), (1e-6, "µ"), (1e-9, "n")):
        if a >= f:
            return f"{v / f:.3g} {p}{unit}"
    return f"0 {unit}" if a < 1e-12 else f"{v:.3g} {unit}"


def _xy_span(a, b):
    ok = lambda p: isinstance(p, list) and len(p) == 2 and all(isinstance(v, (int, float)) for v in p)
    return f"from X{a[0]:g} Y{a[1]:g} to X{b[0]:g} Y{b[1]:g}" if ok(a) and ok(b) else ""


def pcb_tree(info, res):
    sections = []
    airs = [a for a in info.get("airwires") or [] if isinstance(a, dict)]
    unrouted = info.get("unrouted") if isinstance(info.get("unrouted"), int) else len(airs)
    if unrouted:
        rows = [{"name": f"{a.get('net') or 'net'} not routed", "sub": _xy_span(a.get("from"), a.get("to")),
                 "icon": "pcbRoute", "error": True, "ask": f"Route the net {a.get('net')} on the PCB."} for a in airs[:30]]
        if not rows:
            rows = [{"name": f"{unrouted} connection(s) not routed", "icon": "pcbRoute", "error": True, "ask": "Route the PCB's unrouted connections."}]
        sections.append({"title": "Unrouted", "icon": "warning", "count": str(unrouted), "error": True, "rows": rows})
    probes = res.get("probes") if isinstance(res.get("probes"), dict) else {}
    if probes or res.get("error"):
        rows = []
        if res.get("error"):
            rows.append({"name": str(res["error"]), "icon": "warning", "error": True, "wrap": True, "ask": f"The circuit simulation fails: {res['error']} Fix it."})
        for name, pr in probes.items():
            if not isinstance(pr, dict):
                continue
            u = pr.get("unit") or ""
            bits = [f"{_eng(pr.get('min'), u)} to {_eng(pr.get('max'), u)}" if "min" in pr else "",
                    _eng(pr["frequency"], "Hz") if isinstance(pr.get("frequency"), (int, float)) else "",
                    f"duty {pr['duty'] * 100:.0f} %" if isinstance(pr.get("duty"), (int, float)) else ""]
            rows.append({"name": f"{name} average {_eng(pr.get('average'), u)}" if "average" in pr else f"{name} {_eng(pr.get('value'), u)}",
                         "sub": " · ".join(b for b in bits if b), "icon": "pcbSim", "ask": f"In the circuit, the probe {name}: "})
        for note in res.get("notes") or []:
            if isinstance(note, str):
                rows.append({"name": note, "icon": "note", "wrap": True, "ask": f"About the circuit simulation: {note} "})
        sections.append({"title": "Circuit", "icon": "pcbSim", "count": res.get("analysis") or "", "rows": rows})
    parts = [p for p in info.get("parts") or [] if isinstance(p, dict)]
    sections.append({"title": "Parts", "icon": "pcbPlace", "count": str(len(parts)), "empty": "No parts yet.", "rows": [
        {"name": f"{p.get('reference')} {p.get('value') or ''}".strip(),
         "sub": " · ".join(x for x in [p.get("footprint") or "", (p.get("bom") or {}).get("mpn") or "",
                                       "bottom" if p.get("side") == "bottom" else "", "not fitted" if p.get("fitted") is False else ""] if x),
         "icon": "pcb", "dim": p.get("fitted") is False, "ask": f"On the PCB, {p.get('reference')} ({p.get('value')}): "} for p in parts]})
    nets = [n for n in info.get("nets") or [] if isinstance(n, dict)]
    lengths = {x.get("net"): x.get("length") for x in info.get("lengths") or [] if isinstance(x, dict)}
    if nets:
        sections.append({"title": "Nets", "icon": "pcbWire", "count": str(len(nets)), "rows": [
            {"name": n.get("name") or "", "sub": " · ".join(x for x in [", ".join(n.get("pins") or [])[:80],
                                                                        f"{lengths[n.get('name')]:.1f} mm" if isinstance(lengths.get(n.get("name")), (int, float)) else ""] if x),
             "icon": "pcbWire", "ask": f"On the PCB, the net {n.get('name')}: "} for n in nets]})
    b = info.get("board") if isinstance(info.get("board"), dict) else {}
    size = b.get("size") if isinstance(b.get("size"), list) and len(b.get("size")) == 2 else None
    summary = " · ".join(x for x in [f"{size[0]:g} × {size[1]:g} mm" if size else "", f"{b['layers']} layers" if b.get("layers") else "",
                                      f"{len(parts)} parts", f"{b['tracks']} tracks" if b.get("tracks") else "",
                                      f"{unrouted} unrouted" if unrouted else "all routed" if parts else ""] if x)
    return {"kind": "pcb", "name": info.get("name"), "summary": summary, "icon": "pcb",
            "pages": [{"id": "", "label": "Board"}, {"id": "3d", "label": "3D"}], "sections": sections}


def _at(p):
    return f"at X{p[0]:g} Y{p[1]:g} Z{p[2]:g}" if isinstance(p, list) and len(p) == 3 else ""


def sim_tree(study, res):
    """A Simulation tab for the tray: the results, the modes, the supports and loads, material and mesh."""
    analysis = res.get("analysis") or study.get("analysis") or "static"
    rows = []
    ask = "In the simulation's results: "
    if analysis == "static":
        fos = res.get("factor_of_safety")
        if isinstance(fos, (int, float)):
            rows.append({"name": f"Factor of safety {fos:.3g}", "sub": "yield / max stress" + (" · below 1.5" if fos < 1.5 else ""),
                         "icon": "warning" if fos < 1.5 else "check", "error": fos < 1.5, "ask": "Make the part stronger: the factor of safety is only %.3g. " % fos if fos < 1.5 else ask})
        for key, label, unit, at in (("max_von_mises_mpa", "Max von Mises stress", "MPa", "max_stress_at"),
                                     ("max_displacement_mm", "Max displacement", "mm", "max_displacement_at"),
                                     ("max_principal_mpa", "Max principal stress", "MPa", "max_principal_at"),
                                     ("min_principal_mpa", "Min principal stress", "MPa", "min_principal_at")):
            if isinstance(res.get(key), (int, float)):
                rows.append({"name": f"{label} {res[key]:.4g} {unit}", "sub": _at(res.get(at)), "icon": "feaResults", "ask": ask})
        for rc in res.get("reactions") or []:
            if isinstance(rc, dict):
                rows.append({"name": f"Reaction at {rc.get('support')}: {rc.get('magnitude_n', 0):.4g} N", "icon": "feaFixed", "ask": ask})
    elif analysis == "transient":
        for key, label, unit, when in (("peak_von_mises_mpa", "Peak von Mises stress", "MPa", "peak_stress_at_s"),
                                       ("peak_displacement_mm", "Peak displacement", "mm", "peak_at_s"),
                                       ("final_displacement_mm", "Displacement at the end", "mm", None)):
            if isinstance(res.get(key), (int, float)):
                t = res.get(when) if when else None
                rows.append({"name": f"{label} {res[key]:.4g} {unit}", "sub": f"at {t * 1000:.3g} ms" if isinstance(t, (int, float)) else "",
                             "icon": "feaResults", "ask": ask})
        if isinstance(res.get("lowest_frequency_hz"), (int, float)):
            rows.append({"name": f"Lowest natural frequency {_hz(res['lowest_frequency_hz'])}", "icon": "feaResults", "ask": ask})
    sections = []
    errs = [w for w in res.get("warnings") or [] if isinstance(w, str)]
    if res.get("error"):
        errs.insert(0, str(res["error"]))
    if errs:
        sections.append({"title": "Notes", "icon": "note", "count": str(len(errs)),
                         "rows": [{"name": w, "icon": "note", "wrap": True, "ask": f"About the simulation: {w} "} for w in errs]})
    if rows:
        sections.append({"title": "Results", "icon": "feaResults", "count": "", "rows": rows})
    modes = [m for m in res.get("modes") or [] if isinstance(m, dict)]
    if modes:
        sections.append({"title": "Modes", "icon": "feaResults", "count": str(len(modes)), "rows": [
            {"name": f"Mode {m.get('mode')} · {_hz(m.get('frequency_hz') or 0)}",
             "sub": " · ".join(x for x in [f"60 dB decay {m['decay_60db_s']:.3g} s" if isinstance(m.get("decay_60db_s"), (int, float)) else "",
                                           "mass X/Y/Z " + "/".join(f"{v * 100:.0f}" for v in m["mass_participation_xyz"]) + " %"
                                           if isinstance(m.get("mass_participation_xyz"), list) else ""] if x),
             "icon": "feaResults", "ask": f"About mode {m.get('mode')} ({_hz(m.get('frequency_hz') or 0)}): "} for m in modes]})
    icons = {"fixed": "feaFixed", "roller": "feaRoller", "force": "feaForce", "pressure": "feaPressure", "gravity": "feaGravity"}
    setup = []
    for sp in study.get("supports") or []:
        if isinstance(sp, dict):
            setup.append({"name": sp.get("name") or sp.get("kind"), "sub": f"{sp.get('kind')} · {len(sp.get('faces') or [])} face(s)",
                          "icon": icons.get(sp.get("kind"), "feaFixed"), "ask": f"In the support \"{sp.get('name')}\": "})
    for ld in study.get("loads") or []:
        if isinstance(ld, dict):
            unit = {"force": "N", "pressure": "MPa", "gravity": "g"}.get(ld.get("kind"), "")
            val = ld.get("value")
            setup.append({"name": ld.get("name") or ld.get("kind"),
                          "sub": " · ".join(x for x in [str(ld.get("kind")), f"{val:g} {unit}".strip() if isinstance(val, (int, float)) else "",
                                                        "normal to the face" if ld.get("normal") else ""] if x),
                          "icon": icons.get(ld.get("kind"), "feaForce"), "ask": f"In the load \"{ld.get('name')}\": "})
    if setup:
        sections.append({"title": "Supports and loads", "icon": "feaForce", "count": str(len(setup)), "rows": setup})
    mat = res.get("material_props") if isinstance(res.get("material_props"), dict) else {}
    mesh = res.get("mesh") if isinstance(res.get("mesh"), dict) else {}
    info = []
    if res.get("material") or study.get("material"):
        info.append({"name": str(res.get("material") or study.get("material")),
                     "sub": " · ".join(x for x in [f"E {mat['E_mpa'] / 1000:.4g} GPa" if mat.get("E_mpa") else "",
                                                   f"yield {mat['yield_mpa']:g} MPa" if mat.get("yield_mpa") else "",
                                                   f"{res['mass_kg'] * 1000:.4g} g" if isinstance(res.get("mass_kg"), (int, float)) else ""] if x),
                     "icon": "properties", "ask": "Change the simulation's material to "})
    if mesh:
        info.append({"name": f"{mesh.get('elements', 0):,} elements · {mesh.get('nodes', 0):,} nodes",
                     "sub": f"{mesh.get('element') or 'mesh'} · {mesh.get('element_size_mm', 0):g} mm", "icon": "feaMesh",
                     "ask": "Change the simulation's mesh: "})
    if info:
        sections.append({"title": "Material and mesh", "icon": "feaMesh", "count": "", "rows": info})
    if analysis == "static" and isinstance(res.get("max_von_mises_mpa"), (int, float)):
        head = [f"max {res['max_von_mises_mpa']:.3g} MPa",
                f"safety {res['factor_of_safety']:.3g}" if isinstance(res.get("factor_of_safety"), (int, float)) else "",
                f"{res['max_displacement_mm']:.3g} mm" if isinstance(res.get("max_displacement_mm"), (int, float)) else ""]
    elif modes:
        head = [f"{len(modes)} modes from {_hz(modes[0].get('frequency_hz') or 0)}"]
    elif analysis == "transient" and isinstance(res.get("peak_von_mises_mpa"), (int, float)):
        head = [f"peak {res['peak_von_mises_mpa']:.3g} MPa", f"{res.get('peak_displacement_mm', 0):.3g} mm"]
    else:
        head = ["not solved" if not res else ""]
    summary = " · ".join(x for x in [analysis.capitalize(), str(res.get("material") or study.get("material") or "")] + head if x)
    return {"kind": "simulation", "name": study.get("name"), "summary": summary, "icon": "simulation",
            "pages": [{"id": "", "label": "3D"}, {"id": "pictures", "label": "Pictures"}], "sections": sections}


def drawing_tree(info, sheet):
    """A Drawing tab for the tray: its views, dimensions, notes, GD&T and title block, for one sheet."""
    sheets = info.get("sheets") if isinstance(info.get("sheets"), int) else 1
    on = lambda it: (it.get("sheet") or 1) == sheet
    views = [v for v in info.get("views") or [] if isinstance(v, dict) and on(v)]
    view_ids = {v.get("id") for v in views}
    names = {}
    for v in views:
        d = str(v.get("dir") or "view")
        if d.startswith("section"):
            n = f"Section {v.get('letter') or ''}".strip()
        elif v.get("detail_of") is not None:
            n = f"Detail {v.get('letter') or ''}".strip()
        else:
            n = d.replace("_", " ").capitalize()
        names[v.get("id")] = n
    sections = []
    warns = [w if isinstance(w, str) else json.dumps(w) for w in info.get("warnings") or []]
    if warns:
        sections.append({"title": "Problems", "icon": "warning", "count": str(len(warns)), "error": True,
                         "rows": [{"name": w, "icon": "warning", "error": True, "wrap": True, "ask": f"In the drawing: {w} Fix it."} for w in warns]})
    sections.append({"title": "Views", "icon": "view", "count": str(len(views)), "empty": "No views on this sheet.", "rows": [
        {"name": names[v.get("id")],
         "sub": " · ".join(x for x in [f"scale {v['scale']:g}:1" if isinstance(v.get("scale"), (int, float)) else "",
                                       "assembly" if v.get("assembly") is not None else "",
                                       "hidden lines" if v.get("hidden_lines") else ""] if x),
         "icon": "view", "ask": f"In the drawing's {names[v.get('id')].lower()} view: "} for v in views]})
    dims = [d for d in info.get("dimensions") or [] if isinstance(d, dict) and d.get("view") in view_ids]
    if dims:
        icon = {"diameter": "dimRadial", "radius": "dimRadial", "angle": "dimAngle"}
        sections.append({"title": "Dimensions", "icon": "dimension", "count": str(len(dims)), "rows": [
            {"name": d.get("text") or "", "sub": f"{d.get('kind')} · {names.get(d.get('view'), 'view')} view" + (" · lost its geometry" if d.get("dangling") else ""),
             "icon": icon.get(d.get("kind"), "dimension"), "error": bool(d.get("dangling")),
             "ask": f"In the drawing, the dimension {d.get('text')} ({d.get('kind')}): "} for d in dims]})
    gdt = [g for g in info.get("gdt") or [] if isinstance(g, dict) and on(g)]
    if gdt:
        sections.append({"title": "GD&T", "icon": "gdt", "count": str(len(gdt)), "rows": [
            {"name": f"Datum {g.get('tolerance')}" if g.get("kind") == "datum" else f"{g.get('kind')} {g.get('tolerance') or ''}".strip(),
             "sub": f"datums {g['datums']}" if g.get("datums") else "", "icon": "datum" if g.get("kind") == "datum" else "gdt",
             "ask": "In the drawing's GD&T: "} for g in gdt]})
    notes = [n for n in info.get("notes") or [] if isinstance(n, dict) and on(n)]
    if notes:
        sections.append({"title": "Notes", "icon": "note", "count": str(len(notes)), "rows": [
            {"name": n.get("text") or "", "icon": "note", "wrap": True, "ask": f"In the drawing's note \"{n.get('text')}\": "} for n in notes]})
    tb = info.get("title_block") if isinstance(info.get("title_block"), dict) else {}
    labels = [("part", "Part"), ("part_no", "Part number"), ("material", "Material"), ("revision", "Revision"),
              ("tolerance", "General tolerance"), ("drawn_by", "Drawn by"), ("date", "Date")]
    rows = [{"name": str(tb[k]), "sub": lab, "icon": "properties", "ask": f"In the drawing's title block, set the {lab.lower()} to "}
            for k, lab in labels if tb.get(k)]
    if rows:
        sections.append({"title": "Title block", "icon": "properties", "count": "", "rows": rows})
    summary = " · ".join(x for x in [f"{info.get('sheet_size')}" if info.get("sheet_size") else "",
                                      f"sheet {sheet} of {sheets}", f"{len(dims)} dimension{'s' if len(dims) != 1 else ''}",
                                      "first-angle" if tb.get("first_angle") else "third-angle" if tb else ""] if x)
    return {"kind": "drawing", "name": info.get("name"), "summary": summary, "icon": "drawing",
            "pages": [{"id": "" if n == 1 else str(n), "label": f"Sheet {n}"} for n in range(1, max(1, sheets) + 1)],
            "sections": sections}


CAM_OP_ICONS = {"face": "camFace", "pocket": "camPocket", "contour": "camContour", "drill": "camDrill", "thread": "camThread",
                "tap": "camTap", "adaptive": "camAdaptive", "parallel": "camParallel", "waterline": "camWaterline",
                "scallop": "camScallop", "deburr": "camDeburr", "pencil": "camPencil", "trace": "camTrace", "vcarve": "camVCarve",
                "chamfer": "camDeburr", "probe": "camProbe", "clear": "camClear"}


def cam_tree(info, sim):
    """A Manufacture tab for the tray: the simulation's problems, operations with their times and
    warnings, tools, and what the operator does before running."""
    ops = [o for o in info.get("operations") or [] if isinstance(o, dict)]
    tools = {t.get("id"): t for t in info.get("tools") or [] if isinstance(t, dict)}
    problems = [p for p in sim.get("problems") or [] if isinstance(p, dict)]
    sections = []
    errs = [e if isinstance(e, str) else json.dumps(e) for e in info.get("errors") or []]
    if problems or errs or sim.get("ok") is False:
        # one row per kind of problem and operation: a pocket can overload its tool fifty times
        groups = {}
        for p in problems:
            k = (p.get("operation") or "", p.get("what") or p.get("kind") or "problem")
            groups.setdefault(k, []).append(p)
        rows = [{"name": e, "icon": "warning", "error": True, "ask": f"Fix this in the Manufacture tab: {e}"} for e in errs]
        for (op, what), ps in groups.items():
            at = ps[0].get("at")
            where = f" first at X{at[0]:g} Y{at[1]:g} Z{at[2]:g}" if isinstance(at, list) and len(at) == 3 else ""
            n = f"{len(ps)}× " if len(ps) > 1 else ""
            rows.append({"name": f"{n}{what}", "sub": f"{op}{where}", "icon": "warning", "error": True,
                         "ask": f"The CAM simulation finds {len(ps)} problem(s) in '{op}': {what}{where}. Fix it."})
        unf = sim.get("unfinished")
        if isinstance(unf, dict) and unf.get("warning"):
            rows.append({"name": "Unfinished surface", "sub": unf["warning"], "icon": "warning", "error": True,
                         "ask": f"The CAM simulation says: {unf['warning']} Fix it."})
        sections.append({"title": "Problems", "icon": "warning", "count": str(len(problems) + len(errs)), "error": True, "rows": rows})
    op_rows = []
    for o in ops:
        t = tools.get(o.get("tool")) or {}
        bits = [f"T{o.get('tool_number')}" if o.get("tool_number") is not None else "", t.get("description") or "",
                _mins(o.get("time_s")) if o.get("time_s") is not None else "", "suppressed" if o.get("suppressed") else ""]
        warn = [w for w in o.get("warnings") or [] if isinstance(w, str)]
        op_rows.append({"name": o.get("name") or o.get("kind"), "sub": warn[0] if warn else " · ".join(b for b in bits if b),
                        "icon": CAM_OP_ICONS.get(o.get("kind"), "cam"), "error": bool(warn), "dim": bool(o.get("suppressed")),
                        "ask": f"In the operation \"{o.get('name')}\": " + (f"it warns: {' '.join(warn)} Fix it." if warn else "")})
    sections.append({"title": "Operations", "icon": "cam", "count": str(len(ops)), "rows": op_rows,
                     "empty": "No operations yet. Ask Claude to add some."})
    if tools:
        sections.append({"title": "Tools", "icon": "camTools", "count": str(len(tools)), "rows": [
            {"name": f"T{t.get('number')} {t.get('description') or t.get('name')}",
             "sub": " · ".join(x for x in [f"{t['rpm']:g} rpm" if t.get("rpm") else "", f"feed {t['feed']:g}" if t.get("feed") else "",
                                           f"stickout {t['stickout']:g}" if t.get("stickout") else ""] if x),
             "icon": "camTools", "ask": f"For tool T{t.get('number')}: "} for t in tools.values()]})
    notes = [n for n in info.get("setup_notes") or [] if isinstance(n, str)]
    if notes:
        sections.append({"title": "Before running", "icon": "camSetup", "count": str(len(notes)),
                         "rows": [{"name": n, "icon": "note", "wrap": True} for n in notes]})
    removed = sim.get("removed_mm3")
    summary = " · ".join(x for x in [
        _mins(info.get("cycle_time_s")) + " cycle" if info.get("cycle_time_s") else "",
        f"{len(ops)} operation{'s' if len(ops) != 1 else ''}",
        f"{removed / 1000:.1f} cm³ removed" if isinstance(removed, (int, float)) and removed else "",
        info.get("post") or ""] if x)
    return {"kind": "cam", "name": info.get("name"), "summary": summary, "icon": "cam",
            "pages": [{"id": "", "label": "Simulation"}, {"id": "sheet", "label": "Setup sheet"}],
            "sections": sections}


def handle(h, method, p, projects, body=None):
    """Route a request of the receiver's handler `h` (path parts `p`, already authorised; `body` the
    parsed JSON of a POST). True if handled."""
    body = body or {}
    q = urllib.parse.parse_qs(urllib.parse.urlparse(h.path).query)
    qi = lambda k: int(q[k][0]) if k in q and q[k][0].lstrip("-").isdigit() else None
    if p == ["api", "projects"]:
        if method == "GET":
            h.send_json(200, projects.list_docs())
        else:
            did, err = projects.new_project(body.get("name"))
            h.send_json(200 if did else 400, {"id": did} if did else {"error": err})
        return True
    if p[:2] == ["api", "github"]:
        rest = p[2:]
        if method == "GET" and rest == []:
            code, res = projects.github_status(fresh="fresh" in q)
        elif method == "GET" and rest == ["repos"]:
            code, res = projects.github_repos()
        elif method == "POST" and rest == ["login"]:
            code, res = projects.github_login()
        elif method == "POST" and rest == ["login", "poll"]:
            code, res = projects.github_poll()
        elif method == "POST" and rest == ["logout"]:
            code, res = projects.github_logout()
        elif method == "POST" and rest == ["clone"]:
            code, res = projects.github_clone(body.get("url"))
        else:
            return False
        h.send_json(code, res)
        return True
    if len(p) >= 3 and p[:2] == ["api", "doc"]:
        did = p[2]
        if method == "POST" and p[3:] == ["sync"]:
            h.send_json(*projects.doc_git(did, ["sync", "--message", "Synced from the rCAD phone app"]))
            return True
        if method == "POST" and p[3:] == ["resolve"]:
            keep = body.get("keep")
            if keep not in ("mine", "theirs"):
                h.send_json(400, {"error": "keep mine or theirs"})
            else:
                h.send_json(*projects.doc_git(did, ["resolve", "--keep", keep]))
            return True
        if method == "GET" and len(p) == 3:
            d = projects.doc_detail(did)
            h.send_json(200 if d else 404, d or {"error": "no such document"})
            return True
        kind, index = next(((k, qi(k)) for k in TAB_KINDS if qi(k) is not None), ("studio", 0))
        page = (q.get("page") or [""])[0]
        if method == "GET" and p[3:] == ["view"]:
            out, err = projects.view(did, kind, index, page)
            if err == "building":
                secs, line = projects.progress(did, kind, index, page)
                h.send_html(200, building_page(kind, secs, line))
                return True
            if not out:
                h.send_json(404 if err in ("no such document", "no such tab") else 500, {"error": err})
                return True
            gz = projects.gzipped(out) if "gzip" in (h.headers.get("Accept-Encoding") or "") else None
            if gz:
                h.send_file(gz, "text/html; charset=utf-8", encoding="gzip")
            else:
                h.send_file(out, "text/html; charset=utf-8")
            return True
        if method == "GET" and p[3:] == ["tree"]:
            t, err = projects.tree(did, kind, index, page)
            if t is None:
                h.send_json(404 if err in ("no such document", "no such tab") else 500, {"error": err})
            else:
                h.send_json(200, t)
            return True
        if method == "GET" and len(p) == 5 and p[3] == "chats":
            c = projects.chat(did, p[4])
            h.send_json(200 if c else 404, c or {"error": "no such chat"})
            return True
        if method == "POST" and p[3:] == ["chat"]:
            code, res = projects.start_chat(did, body)
            h.send_json(code, res)
            return True
    if len(p) >= 3 and p[:2] == ["api", "jobs"]:
        job = projects.jobs.get(p[2])
        if not job:
            h.send_json(404, {"error": "no such request"})
            return True
        if method == "GET" and len(p) == 3:
            after = max(0, qi("after") or 0)
            wait = min(25.0, max(0.0, float(qi("wait") if qi("wait") is not None else 25)))
            evs, n, done = job.since(after, wait)
            h.send_json(200, {"events": evs, "next": n, "done": done, "chat_id": job.chat_id, "error": job.error,
                             "prompt": job.prompt})
            return True
        if method == "POST" and p[3:] == ["stop"]:
            projects.stop(p[2])
            h.send_json(200, {"ok": True})
            return True
    return False
