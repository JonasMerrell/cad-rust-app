//! The modelling kernel runs in a child process (this same executable started
//! with `--kernel-worker`). Requests and replies are JSON lines; reply lines are
//! tagged because OpenCascade itself sometimes prints to stdout. If the worker
//! dies mid-rebuild, the feature it was working on is marked as crashed and the
//! rebuild is retried without it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

use serde::{Deserialize, Serialize};

use crate::doc::{Document, FeatId};
use crate::kernel::{self, RebuildResult};

const TAG: &str = "\u{1e}CAD ";

#[derive(Serialize, Deserialize)]
pub enum Request {
    Ping,
    Rebuild { id: u64, doc: Document, skip: Vec<FeatId> },
    Export { id: u64, doc: Document, format: String, path: String },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Reply {
    Ready,
    Progress { id: u64, feature: FeatId },
    Rebuilt { id: u64, result: RebuildResult },
    Exported { id: u64, error: Option<String> },
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
            Request::Rebuild { id, doc, skip } => {
                let result = kernel::rebuild(&doc, &skip, |f| send(&Reply::Progress { id, feature: f }));
                send(&Reply::Rebuilt { id, result });
            }
            Request::Export { id, doc, format, path } => {
                let error = kernel::export(&doc, &format, &path).err();
                send(&Reply::Exported { id, error });
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
    Rebuilt(RebuildResult),
    Exported(Option<String>, String),
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
    /// The rebuild in flight: (id, doc, skip list, feature currently running)
    inflight: Option<(u64, Document, Vec<FeatId>, Option<FeatId>)>,
    queued: Option<Document>,
    export: Option<(u64, String)>,
    pub crashes: u32,
    ctx: Option<egui::Context>,
}

impl KernelClient {
    pub fn new(ctx: egui::Context) -> Self {
        let mut k = KernelClient { proc: None, state: KernelState::Loading, next_id: 1, inflight: None, queued: None, export: None, crashes: 0, ctx: Some(ctx) };
        k.spawn();
        k
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
    }

    fn send(&mut self, r: &Request) {
        if let Some(p) = &mut self.proc {
            let line = serde_json::to_string(r).unwrap_or_default();
            if writeln!(p.stdin, "{line}").and_then(|_| p.stdin.flush()).is_err() {
                // the reader thread will notice the exit
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.inflight.is_some() || self.export.is_some()
    }

    /// Ask for a rebuild; if one is running, the newest request replaces any queued one.
    pub fn rebuild(&mut self, doc: &Document) {
        if self.inflight.is_some() || self.state == KernelState::Loading {
            self.queued = Some(doc.clone());
            return;
        }
        self.start(doc.clone(), vec![]);
    }

    fn start(&mut self, doc: Document, skip: Vec<FeatId>) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::Rebuild { id, doc: doc.clone(), skip: skip.clone() });
        self.inflight = Some((id, doc, skip, None));
        self.state = KernelState::Busy;
    }

    pub fn export(&mut self, doc: &Document, format: &str, path: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&Request::Export { id, doc: doc.clone(), format: format.into(), path: path.clone() });
        self.export = Some((id, path));
    }

    /// Process worker messages; returns finished results.
    pub fn poll(&mut self) -> Vec<Outcome> {
        let mut out = vec![];
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
                    if self.inflight.is_none() {
                        if let Some(d) = self.queued.take() {
                            self.start(d, vec![]);
                        }
                    }
                }
                Ev::Reply(Reply::Progress { id, feature }) => {
                    if let Some(f) = &mut self.inflight {
                        if f.0 == id {
                            f.3 = Some(feature);
                        }
                    }
                }
                Ev::Reply(Reply::Rebuilt { id, result }) => {
                    if self.inflight.as_ref().map(|f| f.0) == Some(id) {
                        self.inflight = None;
                        self.state = KernelState::Ready;
                        out.push(Outcome::Rebuilt(result));
                        if let Some(d) = self.queued.take() {
                            self.start(d, vec![]);
                        }
                    }
                }
                Ev::Reply(Reply::Exported { id, error }) => {
                    if let Some((eid, path)) = self.export.take() {
                        if eid == id {
                            out.push(Outcome::Exported(error, path));
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
                    let retry = self.inflight.take();
                    if self.crashes > 50 {
                        self.state = KernelState::Error;
                        return out;
                    }
                    self.state = KernelState::Loading;
                    self.spawn();
                    if let Some((_, doc, mut skip, running)) = retry {
                        // retry without the feature that crashed, unless a newer request is waiting
                        if let Some(f) = running {
                            skip.push(f);
                        }
                        let doc = self.queued.take().unwrap_or(doc);
                        self.start(doc, skip);
                    }
                    break;
                }
            }
        }
        out
    }
}

impl Drop for KernelClient {
    fn drop(&mut self) {
        if let Some(p) = &mut self.proc {
            let _ = p.child.kill();
        }
    }
}
