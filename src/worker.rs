//! The modelling kernel runs in a child process (this same executable started
//! with `--kernel-worker`). Requests and replies are JSON lines; reply lines are
//! tagged because OpenCascade itself sometimes prints to stdout. If the worker
//! dies mid-rebuild, the feature it was working on is marked as crashed and the
//! rebuild is retried without it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use serde::{Deserialize, Serialize};

use crate::doc::{Document, FeatId};
use crate::kernel::{self, RebuildResult};

const TAG: &str = "\u{1e}CAD ";

#[derive(Serialize, Deserialize)]
pub enum Request {
    Ping,
    /// All Part Studios of the workspace, for Derived features.
    SetStudios { studios: Vec<Document> },
    Interference { id: u64, items: Vec<(usize, crate::doc::BodyId, [f64; 12])> },
    Rebuild { id: u64, tag: usize, doc: Document, skip: Vec<FeatId> },
    Export { id: u64, doc: Document, format: String, path: String },
    /// An assembly's placed parts as STEP (Part Studios from SetStudios), fused into one body with `single_body`.
    ExportAssembly { id: u64, items: Vec<kernel::AsmExportItem>, name: String, path: String, single_body: bool },
    Project { id: u64, tag: usize, key: String, doc: Document, views: Vec<kernel::ProjReq> },
    /// Mass properties, point classification or ray casting on a Part Studio's solids (agent tools).
    Query { id: u64, doc: Document, query: serde_json::Value },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Reply {
    Ready,
    Progress { id: u64, feature: FeatId },
    Rebuilt { id: u64, tag: usize, result: RebuildResult },
    Exported { id: u64, error: Option<String> },
    /// Ok(note about what didn't go as asked) or the error.
    AsmExported { id: u64, result: Result<Option<String>, String> },
    Projected { id: u64, tag: usize, key: String, views: Vec<kernel::ProjView> },
    Interference { id: u64, pairs: Vec<(usize, usize, f64)> },
    Queried { id: u64, result: Result<serde_json::Value, String> },
}

/// Entry point of the worker process.
pub fn worker_main() {
    let stdin = std::io::stdin();
    let send = |r: &Reply| {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{TAG}{}", serde_json::to_string(r).unwrap_or_default());
        let _ = out.flush();
    };
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Request>(&line) else { continue };
        match req {
            Request::Ping => send(&Reply::Ready),
            Request::Interference { id, items } => {
                let pairs = kernel::interference(&items);
                send(&Reply::Interference { id, pairs });
            }
            Request::SetStudios { studios } => {
                if let Ok(mut s) = kernel::STUDIOS.lock() {
                    *s = studios;
                }
            }
            Request::Rebuild { id, tag, doc, skip } => {
                let result = kernel::rebuild(&doc, &skip, |f| send(&Reply::Progress { id, feature: f }));
                send(&Reply::Rebuilt { id, tag, result });
            }
            Request::Export { id, doc, format, path } => {
                let error = kernel::export(&doc, &format, &path).err();
                send(&Reply::Exported { id, error });
            }
            Request::ExportAssembly { id, items, name, path, single_body } => {
                let result = kernel::export_assembly(&items, &name, &path, single_body);
                send(&Reply::AsmExported { id, result });
            }
            Request::Project { id, tag, key, doc, views } => {
                let views = kernel::project(&doc, &views);
                send(&Reply::Projected { id, tag, key, views });
            }
            Request::Query { id, doc, query } => {
                let result = crate::agent::query::run(&doc, &query);
                send(&Reply::Queried { id, result });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelState {
    Loading,
    Ready,
    Busy,
    Error,
}

pub enum Outcome {
    /// (part studio index, result)
    Rebuilt(usize, RebuildResult),
    Exported(Option<String>, String),
    /// An assembly STEP export: Ok(note) or the error, and the path.
    AsmExported(Result<Option<String>, String>, String),
    /// (part studio, request key, projections)
    Projected(usize, String, Vec<kernel::ProjView>),
    /// Overlapping instance pairs (indices into the request) and volumes.
    Interference(Vec<(usize, usize, f64)>),
    Queried(Result<serde_json::Value, String>),
}

enum Ev {
    Reply(Reply),
    Exited,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Ev>,
}

pub struct KernelClient {
    proc: Option<Proc>,
    pub state: KernelState,
    next_id: u64,
    /// The rebuild in flight: (id, tag, doc, skip list, feature currently running)
    inflight: Option<(u64, usize, Document, Vec<FeatId>, Option<FeatId>)>,
    /// Waiting rebuilds, newest per tag.
    queued: Vec<(usize, Document)>,
    /// Rebuild requests with smaller ids belong to a document or Part Studio numbering that is
    /// gone (another file opened, a studio deleted, …): their results are dropped.
    stale_before: u64,
    export: Option<(u64, String)>,
    /// The geometry query in flight.
    querying: Option<u64>,
    pub crashes: u32,
    /// Features that crashed the worker: (part studio, feature, fingerprint of the feature and
    /// everything before it). They stay skipped until that changes, so later rebuilds don't
    /// crash on them again.
    crashed: Vec<(usize, FeatId, u64)>,
    /// Last Part Studio list sent, re-sent when the worker restarts.
    studios: Vec<Document>,
    studios_json: String,
    ctx: Option<egui::Context>,
    /// Build-cache key of the rebuild in flight (0 when it skips features).
    inflight_key: u64,
    /// Where built geometry is looked up (`build_cache::dirs_for`); empty turns the cache off.
    pub cache_dirs: Vec<PathBuf>,
    /// Cached results being read: (request id, tag, key, document to rebuild if the entry is bad).
    cache_pending: Vec<(u64, usize, u64, Document)>,
    cache_tx: Sender<(u64, Option<RebuildResult>)>,
    cache_rx: Receiver<(u64, Option<RebuildResult>)>,
    /// Rebuilds in flight whose result a cached one has replaced.
    dropped: Vec<u64>,
}

impl KernelClient {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_ctx(Some(ctx))
    }

    /// A kernel without a window to wake (for the agent interface).
    pub fn new_headless() -> Self {
        Self::with_ctx(None)
    }

    fn with_ctx(ctx: Option<egui::Context>) -> Self {
        let mut k = Self::idle(ctx);
        k.spawn();
        k
    }

    /// A client with no worker started yet.
    fn idle(ctx: Option<egui::Context>) -> Self {
        let (cache_tx, cache_rx) = channel();
        KernelClient {
            proc: None,
            state: KernelState::Loading,
            next_id: 1,
            inflight: None,
            queued: vec![],
            stale_before: 0,
            export: None,
            querying: None,
            crashes: 0,
            crashed: vec![],
            ctx,
            studios: vec![],
            studios_json: String::new(),
            inflight_key: 0,
            cache_dirs: vec![],
            cache_pending: vec![],
            cache_tx,
            cache_rx,
            dropped: vec![],
        }
    }

    fn spawn(&mut self) {
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(_) => {
                self.state = KernelState::Error;
                return;
            }
        };
        let child = Command::new(exe).arg("--kernel-worker").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn();
        let Ok(mut child) = child else {
            self.state = KernelState::Error;
            return;
        };
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(json) = line.strip_prefix(TAG) {
                    if let Ok(r) = serde_json::from_str::<Reply>(json) {
                        if tx.send(Ev::Reply(r)).is_err() {
                            return;
                        }
                        if let Some(c) = &ctx {
                            c.request_repaint();
                        }
                    }
                }
            }
            let _ = tx.send(Ev::Exited);
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        });
        self.proc = Some(Proc { child, stdin, rx });
        self.send(&Request::Ping);
        if !self.studios.is_empty() {
            let studios = self.studios.clone();
            self.send(&Request::SetStudios { studios });
        }
    }

    fn send(&mut self, r: &Request) {
        if let Some(p) = &mut self.proc {
            let line = serde_json::to_string(r).unwrap_or_default();
            if writeln!(p.stdin, "{line}").and_then(|_| p.stdin.flush()).is_err() {
                // the reader thread will notice the exit
            }
        }
    }

    /// Tell the worker about every Part Studio (only sends when something changed).
    pub fn set_studios(&mut self, studios: Vec<Document>) {
        let json = serde_json::to_string(&studios).unwrap_or_default();
        if json != self.studios_json {
            self.studios_json = json;
            self.studios = studios.clone();
            self.send(&Request::SetStudios { studios });
        }
    }

    /// Forget every rebuild asked for so far: results still on their way are dropped and queued
    /// requests are discarded. Call it when Part Studio indices stop meaning what they meant.
    pub fn invalidate(&mut self) {
        self.stale_before = self.next_id;
        self.queued.clear();
        self.crashed.clear();
        self.cache_pending.clear();
    }

    /// A rebuild reply arrived: finish the request in flight and keep the result unless stale.
    fn on_rebuilt(&mut self, id: u64, tag: usize, result: RebuildResult, out: &mut Vec<Outcome>) {
        if self.inflight.as_ref().map(|f| f.0) == Some(id) {
            self.inflight = None;
            self.state = KernelState::Ready;
            log::info!("Part Studio {tag} built by the kernel");
            let replaced = self.dropped.contains(&id);
            self.dropped.retain(|d| *d != id);
            if id >= self.stale_before && !replaced {
                let mut result = result;
                result.key = self.inflight_key;
                out.push(Outcome::Rebuilt(tag, result));
            }
            self.start_next();
        }
    }

    pub fn busy(&self) -> bool {
        self.inflight.is_some() || self.export.is_some()
    }

    /// The build-cache key of `doc` as a rebuild now would see it.
    pub fn cache_key(&self, doc: &Document) -> u64 {
        crate::build_cache::key(doc, || self.studios_json.clone())
    }

    /// Ask for a rebuild of part studio `tag`; a newer request replaces a queued one for the same tag.
    /// Geometry built before (see `build_cache`) is read back instead when the cache has it.
    pub fn rebuild(&mut self, tag: usize, doc: &Document) {
        // a newer request replaces one still reading the cache
        self.cache_pending.retain(|p| p.1 != tag);
        if !self.cache_dirs.is_empty() {
            let key = self.cache_key(doc);
            if let Some(path) = crate::build_cache::find(&self.cache_dirs, key) {
                self.queued.retain(|q| q.0 != tag);
                if let Some(f) = self.inflight.as_ref().filter(|f| f.1 == tag) {
                    self.dropped.push(f.0);
                }
                let id = self.next_id;
                self.next_id += 1;
                self.cache_pending.push((id, tag, key, doc.clone()));
                let (tx, ctx) = (self.cache_tx.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let t = std::time::Instant::now();
                    let r = crate::build_cache::load(&path, key);
                    log::info!("Part Studio {tag}: built geometry read from {} in {:?}", path.display(), t.elapsed());
                    if r.is_none() {
                        log::warn!("ignoring a damaged build cache entry {}", path.display());
                        let _ = std::fs::remove_file(&path);
                    }
                    let _ = tx.send((id, r));
                    if let Some(c) = ctx {
                        c.request_repaint();
                    }
                });
                return;
            }
        }
        self.rebuild_now(tag, doc);
    }

    fn rebuild_now(&mut self, tag: usize, doc: &Document) {
        if self.inflight.is_some() || self.state == KernelState::Loading {
            self.queued.retain(|q| q.0 != tag);
            self.queued.push((tag, doc.clone()));
            return;
        }
        self.start(tag, doc.clone(), vec![]);
    }

    fn start_next(&mut self) {
        if self.inflight.is_none() && !self.queued.is_empty() {
            let (tag, d) = self.queued.remove(0);
            self.start(tag, d, vec![]);
        }
    }

    fn start(&mut self, tag: usize, doc: Document, mut skip: Vec<FeatId>) {
        self.crashed.retain(|(t, f, h)| *t != tag || fingerprint(&doc, *f) == Some(*h));
        for (t, f, _) in &self.crashed {
            if *t == tag && !skip.contains(f) {
                skip.push(*f);
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.inflight_key = if skip.is_empty() { self.cache_key(&doc) } else { 0 };
        self.send(&Request::Rebuild { id, tag, doc: doc.clone(), skip: skip.clone() });
        self.inflight = Some((id, tag, doc, skip, None));
        self.state = KernelState::Busy;
    }

    pub fn project<V: Into<kernel::ProjReq>>(&mut self, tag: usize, key: String, doc: &Document, views: Vec<V>) {
        let id = self.next_id;
        self.next_id += 1;
        let views = views.into_iter().map(Into::into).collect();
        self.send(&Request::Project { id, tag, key, doc: doc.clone(), views });
    }

    pub fn interference(&mut self, items: Vec<(usize, crate::doc::BodyId, [f64; 12])>) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::Interference { id, items });
    }

    pub fn export(&mut self, doc: &Document, format: &str, path: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::Export { id, doc: doc.clone(), format: format.into(), path: path.clone() });
        self.export = Some((id, path));
    }

    pub fn export_assembly(&mut self, items: Vec<kernel::AsmExportItem>, name: &str, path: String, single_body: bool) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::ExportAssembly { id, items, name: name.into(), path: path.clone(), single_body });
        self.export = Some((id, path));
    }

    /// Process worker messages; returns finished results.
    pub fn poll(&mut self) -> Vec<Outcome> {
        let mut out = vec![];
        // cached geometry read back
        while let Ok((id, r)) = self.cache_rx.try_recv() {
            let Some(i) = self.cache_pending.iter().position(|p| p.0 == id) else { continue };
            let (_, tag, _, doc) = self.cache_pending.remove(i);
            match r {
                Some(r) => out.push(Outcome::Rebuilt(tag, r)),
                None => self.rebuild_now(tag, &doc),
            }
        }
        loop {
            let ev = match &self.proc {
                Some(p) => match p.rx.try_recv() {
                    Ok(ev) => ev,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => Ev::Exited,
                },
                None => break,
            };
            match ev {
                Ev::Reply(Reply::Ready) => {
                    if self.state == KernelState::Loading {
                        self.state = KernelState::Ready;
                    }
                    self.start_next();
                }
                Ev::Reply(Reply::Progress { id, feature }) => {
                    if let Some(f) = &mut self.inflight {
                        if f.0 == id {
                            f.4 = Some(feature);
                        }
                    }
                }
                Ev::Reply(Reply::Rebuilt { id, tag, result }) => self.on_rebuilt(id, tag, result, &mut out),
                Ev::Reply(Reply::Projected { tag, key, views, .. }) => out.push(Outcome::Projected(tag, key, views)),
                Ev::Reply(Reply::Interference { pairs, .. }) => out.push(Outcome::Interference(pairs)),
                Ev::Reply(Reply::Queried { result, .. }) => out.push(Outcome::Queried(result)),
                Ev::Reply(Reply::Exported { id, error }) => {
                    if let Some((eid, path)) = self.export.take() {
                        if eid == id {
                            out.push(Outcome::Exported(error, path));
                        }
                    }
                }
                Ev::Reply(Reply::AsmExported { id, result }) => {
                    if let Some((eid, path)) = self.export.take() {
                        if eid == id {
                            out.push(Outcome::AsmExported(result, path));
                        }
                    }
                }
                Ev::Exited => {
                    self.crashes += 1;
                    if let Some(mut p) = self.proc.take() {
                        let _ = p.child.kill();
                        let _ = p.child.wait();
                    }
                    if let Some((_, path)) = self.export.take() {
                        out.push(Outcome::Exported(Some("the kernel crashed while exporting".into()), path));
                    }
                    if self.querying.take().is_some() {
                        out.push(Outcome::Queried(Err("the kernel crashed during the query".into())));
                    }
                    // (a stale request is not retried)
                    let stale_before = self.stale_before;
                    let retry = self.inflight.take().filter(|f| f.0 >= stale_before && !self.dropped.contains(&f.0));
                    if self.crashes > 50 {
                        self.state = KernelState::Error;
                        return out;
                    }
                    self.state = KernelState::Loading;
                    self.spawn();
                    if let Some((_, tag, doc, mut skip, running)) = retry {
                        // retry without the feature that crashed, unless a newer request is waiting
                        if let Some(f) = running {
                            skip.push(f);
                            if let Some(h) = fingerprint(&doc, f) {
                                self.crashed.push((tag, f, h));
                            }
                        }
                        let newer = self.queued.iter().position(|q| q.0 == tag).map(|i| self.queued.remove(i).1);
                        match newer {
                            Some(d) => self.start(tag, d, vec![]),
                            None => self.start(tag, doc, skip),
                        }
                    }
                    break;
                }
            }
        }
        out
    }
}

/// A hash of feature `id` with everything it can depend on: the variables, configurations,
/// units and every feature up to it. `None` when the feature is gone.
fn fingerprint(doc: &Document, id: FeatId) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let i = doc.index_of(id)?;
    let json = serde_json::to_string(&(&doc.variables, &doc.configs, doc.active_config, &doc.units, &doc.features[..=i], &doc.custom_features)).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    Some(h.finish())
}

impl KernelClient {
    fn wait_for<T>(&mut self, mut pick: impl FnMut(Outcome) -> Option<T>) -> Result<T, String> {
        let start = std::time::Instant::now();
        loop {
            for o in self.poll() {
                if let Some(t) = pick(o) {
                    return Ok(t);
                }
            }
            if self.state == KernelState::Error {
                return Err("the modelling kernel keeps crashing".into());
            }
            if start.elapsed() > std::time::Duration::from_secs(600) {
                return Err("the kernel timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
    }

    /// Rebuild and wait for the result.
    pub fn rebuild_blocking(&mut self, tag: usize, doc: &Document) -> Result<RebuildResult, String> {
        self.rebuild(tag, doc);
        self.wait_for(|o| match o {
            Outcome::Rebuilt(t, r) if t == tag => Some(r),
            _ => None,
        })
    }

    pub fn export_blocking(&mut self, doc: &Document, format: &str, path: String) -> Result<(), String> {
        self.export(doc, format, path);
        self.wait_for(|o| match o {
            Outcome::Exported(err, _) => Some(err),
            _ => None,
        })?
        .map_or(Ok(()), Err)
    }

    pub fn export_assembly_blocking(&mut self, items: Vec<kernel::AsmExportItem>, name: &str, path: String, single_body: bool) -> Result<Option<String>, String> {
        self.export_assembly(items, name, path, single_body);
        self.wait_for(|o| match o {
            Outcome::AsmExported(r, _) => Some(r),
            Outcome::Exported(Some(e), _) => Some(Err(e)),
            _ => None,
        })?
    }

    pub fn interference_blocking(&mut self, items: Vec<(usize, crate::doc::BodyId, [f64; 12])>) -> Result<Vec<(usize, usize, f64)>, String> {
        self.interference(items);
        self.wait_for(|o| match o {
            Outcome::Interference(p) => Some(p),
            _ => None,
        })
    }

    /// A geometry query on `doc`'s solids (see `agent::query::run`), waited for.
    pub fn query_blocking(&mut self, doc: &Document, query: serde_json::Value) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.querying = Some(id);
        self.send(&Request::Query { id, doc: doc.clone(), query });
        let r = self.wait_for(|o| match o {
            Outcome::Queried(r) => Some(r),
            _ => None,
        });
        self.querying = None;
        r?
    }

    pub fn project_blocking<V: Into<kernel::ProjReq>>(&mut self, tag: usize, key: &str, doc: &Document, views: Vec<V>) -> Result<Vec<kernel::ProjView>, String> {
        self.project(tag, key.to_string(), doc, views);
        self.wait_for(|o| match o {
            Outcome::Projected(t, k, v) if t == tag && k == key => Some(v),
            _ => None,
        })
    }
}

impl Drop for KernelClient {
    fn drop(&mut self) {
        if let Some(p) = &mut self.proc {
            let _ = p.child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> KernelClient {
        let mut k = KernelClient::idle(None);
        k.state = KernelState::Ready;
        k
    }

    #[test]
    fn stale_rebuilds_are_dropped() {
        let mut k = client();
        let doc = Document::default();
        k.rebuild(2, &doc); // in flight (id 1)
        k.rebuild(1, &doc); // queued
        let id = k.inflight.as_ref().unwrap().0;
        // another document is opened: what was asked for before no longer applies
        k.invalidate();
        assert!(k.queued.is_empty());
        let mut out = vec![];
        k.on_rebuilt(id, 2, RebuildResult::default(), &mut out);
        assert!(out.is_empty(), "a stale result must not be applied");
        assert!(k.inflight.is_none());
        // a request made afterwards comes through
        k.rebuild(0, &doc);
        let id = k.inflight.as_ref().unwrap().0;
        k.on_rebuilt(id, 0, RebuildResult::default(), &mut out);
        assert!(matches!(out.as_slice(), [Outcome::Rebuilt(0, _)]));
    }

    #[test]
    fn cached_geometry_replaces_a_rebuild() {
        let dir = std::env::temp_dir().join(format!("rcad-worker-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut k = client();
        let old = Document::default();
        let mut doc = Document::default();
        doc.variables.push(crate::doc::Variable { name: "w".into(), expr: "5".into() });
        // an older version of studio 0 is being built when the cached one is asked for
        k.rebuild(0, &old);
        let old_id = k.inflight.as_ref().unwrap().0;
        let mut cached = RebuildResult::default();
        cached.bodies.push(kernel::BodyMesh { name: "From the cache".into(), ..Default::default() });
        cached.key = k.cache_key(&doc);
        crate::build_cache::store_now(&dir, &cached).unwrap();
        k.cache_dirs = vec![dir.clone()];
        k.rebuild(0, &doc);
        assert!(k.inflight.is_some() && k.cache_pending.len() == 1, "read from the cache, not queued");
        let mut out = vec![];
        let start = std::time::Instant::now();
        while out.is_empty() && start.elapsed() < std::time::Duration::from_secs(10) {
            out = k.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        match out.as_slice() {
            [Outcome::Rebuilt(0, r)] => {
                assert_eq!(r.bodies[0].name, "From the cache");
                assert_eq!(r.key, cached.key);
            }
            _ => panic!("expected the cached result"),
        }
        // the older build finishing afterwards must not replace it
        let mut late = vec![];
        k.on_rebuilt(old_id, 0, RebuildResult::default(), &mut late);
        assert!(late.is_empty());
        // a document the cache doesn't have goes to the kernel, and its result gets its key
        let mut other = doc.clone();
        other.variables[0].expr = "6".into();
        k.rebuild(0, &other);
        let id = k.inflight.as_ref().unwrap().0;
        k.on_rebuilt(id, 0, RebuildResult::default(), &mut late);
        assert!(matches!(late.as_slice(), [Outcome::Rebuilt(0, r)] if r.key == crate::build_cache::key(&other, String::new)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
