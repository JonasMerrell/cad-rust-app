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
                                            "sheet" the setup sheet)
    GET  /api/doc/<id>/tree[?<kind>=N][&page=P]
                                            what the phone lists beside that page: the feature tree,
                                            an assembly's instances and mates, or (other kinds)
                                            "sections" of rows plus the tab's other "pages"
    GET  /api/doc/<id>/chats/<chat>         a saved chat (the same .rcad-chats the editor uses)
    POST /api/doc/<id>/chat                 {"text", "chat_id"?, "studio"?, "model"?, "effort"?}
                                            starts a request: {"job", "chat_id"}
    GET  /api/jobs/<job>?after=N            events after N (waits up to 25 s for new ones)
    POST /api/jobs/<job>/stop

Requests run `rcad chat` (src/claude_cli.rs), which uses this computer's Claude Code login, so the
phone never holds a Claude sign-in or key. Document ids are the path relative to a project folder,
base64url-encoded, prefixed by the folder's index.
"""

import base64
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
# bump when what a cached page holds changes
VIEW_VERSION = 3
# tab kinds a page can show, by query parameter, and their list in the workspace file
TAB_KINDS = {"studio": "studios", "assembly": "assemblies", "cam": "cams"}
# feature params worth showing in the tree, in this order
SHOWN_PARAMS = ("depth", "radius", "distance", "thickness", "angle", "count", "count2", "offset", "pitch", "height", "end", "op")
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
                "drawings": len(ws.get("drawings") or []),
                "trees": [feature_tree(s.get("doc")) for s in studios],
                "cams": _names(ws.get("cams"), "Setup"),
            }
        else:  # an old single Part Studio document
            info = {"studios": ["Part Studio 1"], "assemblies": [], "drawings": 0, "trees": [feature_tree(ws)], "cams": []}
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

    def _run_script(self, rcad, steps, cwd, timeout, results=None):
        """Run tool calls with `rcad run`; an error text, or None. `results` (a list) gets each step's result."""
        fd, sp = tempfile.mkstemp(suffix=".json", prefix="rcad-mobile-")
        with os.fdopen(fd, "w") as f:
            json.dump(steps, f)
        try:
            # no --stop-on-error: a document whose features fail still opens and exports
            r = subprocess.run([rcad, "run", sp], cwd=cwd, capture_output=True, text=True, timeout=timeout)
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

    def view(self, did, kind="studio", index=0, page=""):
        """Path of the page showing one tab of a document (made with rCAD when the document changed),
        or (None, error). The same run writes what the phone lists beside it (see `tree`)."""
        path = self.resolve(did)
        if not path:
            return None, "no such document"
        if kind not in TAB_KINDS:
            return None, f"can't show a {kind} tab"
        rcad = self.rcad
        if not rcad:
            return None, "rcad is not installed on this computer"
        index = max(0, index or 0)
        have = (self.info(path) or {}).get(TAB_KINDS[kind])
        if isinstance(have, list) and index >= len(have) and not (kind == "studio" and index == 0):
            return None, "no such tab"
        page = re.sub(r"[^a-z0-9]", "", page or "")[:16]
        st = os.stat(path)
        sel = f"{kind}{index}{'-' + page if page else ''}"
        try:  # a new rcad build can change the page (it is compiled in), so it is part of the key
            built = os.stat(rcad).st_mtime_ns
        except OSError:
            built = 0
        key = hashlib.sha256(f"{path}\0{st.st_mtime_ns}\0{st.st_size}\0{sel}\0{VIEW_VERSION}\0{built}".encode()).hexdigest()[:24]
        os.makedirs(CACHE_DIR, exist_ok=True)
        out = os.path.join(CACHE_DIR, f"{key}.html")
        with self.lock:
            lk = self.export_locks.setdefault(key, threading.Lock())
        with lk:
            if os.path.isfile(out) and os.path.isfile(out[:-5] + ".tree.json"):
                return out, None
            tmp = out + ".tmp.html"
            steps, finish = page_steps(kind, index, page, tmp)
            steps.insert(0, {"tool": "open_document", "args": {"path": path}})
            results = []
            err = self._run_script(rcad, steps, os.path.dirname(path), EXPORT_TIMEOUT, results)
            by = {}
            for v in results:
                if isinstance(v.get("result"), dict):
                    by[v["tool"]] = v["result"]
            tree = finish(by, tmp)
            if not os.path.isfile(tmp):
                return None, err or "rcad wrote no page"
            with open(out[:-5] + ".tree.json", "w") as f:
                json.dump(tree, f)
            os.replace(tmp, out)
            self._prune_cache(path, sel, out)
        return out, None

    def tree(self, did, kind="studio", index=0, page=""):
        """What the phone lists beside the page: see `view`."""
        out, err = self.view(did, kind, index, page)
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

    def _gc(self):
        now = time.time()
        for jid in [j for j, job in self.jobs.items() if job.done and now - (job.finished_at or now) > JOB_KEEP_SECS]:
            del self.jobs[jid]


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


def page_steps(kind, index, page, out):
    """The rcad run steps that write the page for one tab to `out`, and a function that turns their
    results (by tool name) into what the phone lists beside it."""
    if kind == "assembly":
        return ([{"tool": "assembly_info", "args": {"assembly": index}},
                 {"tool": "export_web", "args": {"path": out, "assembly": index}}],
                lambda by, _out: assembly_tree(by.get("assembly_info") or {}))
    if kind == "cam":
        export = ({"tool": "export_setup_sheet", "args": {"cam": index, "path": out}} if page == "sheet"
                  else {"tool": "export_cam_simulation", "args": {"cam": index, "path": out}})
        return ([{"tool": "cam_info", "args": {"cam": index}}, {"tool": "simulate_cam", "args": {"cam": index}}, export],
                lambda by, _out: cam_tree(by.get("cam_info") or {}, by.get("simulate_cam") or {}))
    steps = [{"tool": "switch_part_studio", "args": {"index": index}}] if index else []
    steps += [{"tool": "describe", "args": {}}, {"tool": "export_web", "args": {"path": out, "studio": index}}]
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
    if len(p) >= 3 and p[:2] == ["api", "doc"]:
        did = p[2]
        if method == "GET" and len(p) == 3:
            d = projects.doc_detail(did)
            h.send_json(200 if d else 404, d or {"error": "no such document"})
            return True
        kind, index = next(((k, qi(k)) for k in TAB_KINDS if qi(k) is not None), ("studio", 0))
        page = (q.get("page") or [""])[0]
        if method == "GET" and p[3:] == ["view"]:
            out, err = projects.view(did, kind, index, page)
            if not out:
                h.send_json(404 if err in ("no such document", "no such tab") else 500, {"error": err})
                return True
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
