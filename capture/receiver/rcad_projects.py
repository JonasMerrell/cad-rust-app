"""rCAD projects for the phone app: list documents on netdash, show them in rCAD's web viewer,
and chat with Claude about them.

Part of the netdash receiver (rcad_capture_receiver.py serves these routes on the same port and
token as the Capture app). Standard library only.

    GET  /api/projects                      documents in the project folders
    POST /api/projects                      {"name"}: a new project with an empty document
    GET  /api/doc/<id>                      one document: tabs, chats, whether Claude is busy on it
    GET  /api/doc/<id>/view[?assembly=N|studio=N][&t=<token>]
                                            rCAD's interactive web page of it (export_web), cached
    GET  /api/doc/<id>/tree[?assembly=N|studio=N]
                                            the feature tree (or an assembly's instances and mates) of that page
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
VIEW_VERSION = 2
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
            info = {
                "studios": [s.get("name", f"Part Studio {n + 1}") for n, s in enumerate(ws.get("studios") or [])],
                "assemblies": [a.get("name", f"Assembly {n + 1}") for n, a in enumerate(ws.get("assemblies") or [])],
                "drawings": len(ws.get("drawings") or []),
            }
        else:  # an old single Part Studio document
            info = {"studios": ["Part Studio 1"], "assemblies": [], "drawings": 0}
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

    def view(self, did, assembly=None, studio=None):
        """Path of the web page for a document (made with rCAD's export_web when the document changed), or (None, error).
        The same run writes the feature tree beside it (see `tree`)."""
        path = self.resolve(did)
        if not path:
            return None, "no such document"
        rcad = self.rcad
        if not rcad:
            return None, "rcad is not installed on this computer"
        st = os.stat(path)
        sel = f"a{assembly}" if assembly is not None else f"s{studio or 0}"
        key = hashlib.sha256(f"{path}\0{st.st_mtime_ns}\0{st.st_size}\0{sel}\0{VIEW_VERSION}".encode()).hexdigest()[:24]
        os.makedirs(CACHE_DIR, exist_ok=True)
        out = os.path.join(CACHE_DIR, f"{key}.html")
        with self.lock:
            lk = self.export_locks.setdefault(key, threading.Lock())
        with lk:
            if os.path.isfile(out) and os.path.isfile(out[:-5] + ".tree.json"):
                return out, None
            args = {"path": out + ".tmp.html"}
            steps = [{"tool": "open_document", "args": {"path": path}}]
            if assembly is not None:
                args["assembly"] = assembly
                steps.append({"tool": "assembly_info", "args": {"assembly": assembly}})
            else:
                args["studio"] = studio or 0
                if studio:
                    steps.append({"tool": "switch_part_studio", "args": {"index": studio}})
                steps.append({"tool": "describe", "args": {}})
            steps.append({"tool": "export_web", "args": args})
            results = []
            err = self._run_script(rcad, steps, os.path.dirname(path), EXPORT_TIMEOUT, results)
            if not os.path.isfile(args["path"]):
                return None, err or "export_web wrote nothing"
            tree = make_tree(results, assembly)
            with open(out[:-5] + ".tree.json", "w") as f:
                json.dump(tree, f)
            os.replace(args["path"], out)
            self._prune_cache(path, sel, out)
        return out, None

    def tree(self, did, assembly=None, studio=None):
        """The feature tree (Part Studio) or instances and mates (Assembly) the viewer page was made with."""
        out, err = self.view(did, assembly, studio)
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


def make_tree(results, assembly):
    """What the phone shows beside the model, from the results of the viewer run."""
    by = {}
    for v in results:
        if isinstance(v.get("result"), dict):
            by[v["tool"]] = v["result"]
    if assembly is not None:
        a = by.get("assembly_info") or {}
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
    d = by.get("describe") or by.get("open_document") or {}
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
        if method == "GET" and p[3:] == ["view"]:
            out, err = projects.view(did, assembly=qi("assembly"), studio=qi("studio"))
            if not out:
                h.send_json(404 if err == "no such document" else 500, {"error": err})
                return True
            h.send_file(out, "text/html; charset=utf-8")
            return True
        if method == "GET" and p[3:] == ["tree"]:
            t, err = projects.tree(did, assembly=qi("assembly"), studio=qi("studio"))
            if t is None:
                h.send_json(404 if err == "no such document" else 500, {"error": err})
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
