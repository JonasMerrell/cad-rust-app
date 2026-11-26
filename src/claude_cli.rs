//! `rcad chat`: one request to Claude about a saved document, with no window. The rCAD phone app
//! uses it through the netdash service (capture/receiver): the phone sends a request, netdash runs
//! this, and the answer streams back as it arrives.
//!
//! It is the editor's chat panel without the panel: the same worker, tools, instructions and
//! sign-ins (`claude.json`, normally Claude Code with the user's own login), and the chat is saved
//! in the document's `.rcad-chats/` exactly as the panel saves it, so a conversation started on
//! the phone continues in the editor and the other way round.
//!
//! Output is one JSON object per line on stdout:
//!
//! ```text
//! {"type":"chat","id":"…"}                       the chat this request belongs to (first line)
//! {"type":"text","text":"…"}                      Claude's reply, in pieces
//! {"type":"thinking","text":"…"}
//! {"type":"tool_start","name":"…","detail":"…"}
//! {"type":"tool_done","ok":true,"summary":"…","image":"<base64 png>"}   image only for renders
//! {"type":"notice","text":"…"}
//! {"type":"rewind"}                               drop the text since the last "attempt": it is sent again
//! {"type":"attempt"}
//! {"type":"saved","path":"…"}                     the document was written with Claude's changes
//!                                                  (then, in a git project, a notice with what was committed and synced)
//! {"type":"done","error":null}                    always last
//! ```
//!
//! A line `stop` on stdin stops the request (what it changed so far is still saved).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use base64::Engine;
use serde_json::{json, Value};

use crate::claude::{self, Auth, FromWorker, ToWorker};
use crate::claude_chats::{self, Line};

const USAGE: &str =
    "usage: rcad chat --doc part.json [--chat ID] [--studio N] [--auth claude-code|api-key|console] [--model M] [--effort E] [--no-delegate] [--image picture.png|photo.jpg]… (--prompt TEXT | --prompt-file F)";

struct Args {
    doc: PathBuf,
    chat: Option<String>,
    studio: usize,
    prompt: String,
    auth: Option<Auth>,
    model: Option<String>,
    effort: Option<String>,
    no_delegate: bool,
    /// A picture sent with the request (a pencil drawing, a photo), PNG.
    /// Pictures for the request (repeat --image): photos from the phone, a drawing.
    images: Vec<Vec<u8>>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let (mut doc, mut chat, mut studio, mut prompt, mut auth, mut model, mut effort, mut no_delegate) = (None, None, 0, None, None, None, None, false);
    let mut images = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--doc" => doc = Some(PathBuf::from(val()?)),
            "--chat" => chat = Some(val()?).filter(|c| !c.is_empty()),
            "--studio" => studio = val()?.parse().map_err(|_| "--studio takes a number")?,
            "--prompt" => prompt = Some(val()?),
            "--prompt-file" => {
                let p = val()?;
                prompt = Some(std::fs::read_to_string(&p).map_err(|e| format!("can't read {p}: {e}"))?);
            }
            "--auth" => {
                auth = Some(match val()?.as_str() {
                    "claude-code" | "claude_code" => Auth::ClaudeCode,
                    "api-key" | "api_key" | "api" => Auth::ApiKey,
                    "console" => Auth::Console,
                    o => return Err(format!("unknown --auth {o}")),
                })
            }
            "--model" => model = Some(val()?),
            "--effort" => effort = Some(val()?),
            "--no-delegate" => no_delegate = true,
            "--image" => {
                let p = val()?;
                images.push(std::fs::read(&p).map_err(|e| format!("can't read {p}: {e}"))?);
            }
            o => return Err(format!("unknown option {o}")),
        }
    }
    let doc = doc.ok_or("give --doc")?;
    let prompt = prompt.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).ok_or("give --prompt or --prompt-file")?;
    Ok(Args { doc, chat, studio, prompt, auth, model, effort, no_delegate, images })
}

fn emit(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// What Claude is told about where the request comes from (the panel sends the open tab and the selection).
fn context(doc: &std::path::Path, ws: &crate::workspace::Workspace, studio: usize) -> String {
    let name = ws.studios.get(studio).map(|s| s.name.as_str()).unwrap_or("");
    let units = ws.studios.get(studio).map(|s| s.doc.units.label()).unwrap_or("mm");
    format!(
        "Open tab: Part Studio {studio} \"{name}\"\nUnits: {units}\nDocument file: {}\nSelected: nothing\nThe user is on the rCAD phone app: they read your reply on a small screen and see the model in its 3D viewer once you finish, so keep replies short. They can't pick geometry or see renders larger than a thumbnail.",
        doc.display()
    )
}

pub fn main(args: &[String]) -> i32 {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let doc = std::fs::canonicalize(&a.doc).unwrap_or(a.doc.clone());
    // a project on GitHub: bring in what was pushed from elsewhere before Claude starts
    let pulled = crate::github_cli::pull_first(&doc);
    let ws_text = match std::fs::read_to_string(&doc) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("can't open {}: {e}", doc.display());
            return 1;
        }
    };
    let ws = match crate::workspace::Workspace::from_json(&ws_text) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("can't open {}: {e}", doc.display());
            return 1;
        }
    };
    let studio = a.studio.min(ws.studios.len().saturating_sub(1));
    let mut settings = claude::load_settings();
    // the phone has no sign-in of its own: by default the computer's Claude Code login does the work
    settings.auth = a.auth.unwrap_or(Auth::ClaudeCode);
    if let Some(m) = &a.model {
        settings.model = m.clone();
    }
    if let Some(e) = &a.effort {
        settings.effort = e.clone();
    }
    if a.no_delegate {
        settings.delegate = false;
    }

    // the chat to continue, or a new one
    let (mut meta, mut ctx) = match &a.chat {
        Some(id) => match claude_chats::load(&doc, id) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("can't open chat {id}: {e}");
                return 1;
            }
        },
        None => {
            let t = claude_chats::now();
            let m = claude_chats::Meta {
                format: 0,
                id: claude_chats::new_id(),
                document: String::new(),
                title: String::new(),
                created: t,
                updated: t,
                model: settings.model.clone(),
                lines: vec![],
            };
            (m, claude_chats::Context::default())
        }
    };
    meta.model = settings.model.clone();
    emit(json!({"type": "chat", "id": meta.id}));
    if let Some(t) = pulled {
        emit(json!({"type": "notice", "text": t}));
    }

    let worker = claude::Worker::start(egui::Context::default());
    if a.chat.is_some() {
        let recap = Some(claude_chats::recap(&meta.lines)).filter(|r| !r.is_empty());
        let _ = worker.tx.send(ToWorker::Load { messages: ctx.messages.clone(), cc_session: ctx.claude_code_session.clone(), planned: ctx.planned, recap });
    }
    // `stop` on stdin stops the request
    let stop = worker.stop.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line.trim() == "stop" {
                stop.store(true, Ordering::Relaxed);
            }
        }
    });
    // the saved chat (and the editor's panel) says photos came with the request
    let n = a.images.len();
    let shown = if n == 0 { a.prompt.clone() } else { format!("{}\n[{n} photo{} attached]", a.prompt, if n == 1 { "" } else { "s" }) };
    meta.lines.push(Line::User { text: shown });
    let mut ctx_text = context(&doc, &ws, studio);
    if n > 0 {
        ctx_text.push_str(&format!(
            "\nThe user attached {n} photo{} from their phone (shown with the request) as reference: use them for shapes, proportions and features. Sizes in a photo are only a guide unless the user gives dimensions or a known reference is in the shot.",
            if n == 1 { "" } else { "s" }
        ));
    }
    let _ = worker.tx.send(ToWorker::Turn { text: a.prompt.clone(), context: ctx_text, ws: ws_text.clone(), studio, settings, images: a.images.clone() });

    let mut changed: Option<String> = None;
    // where the request now being sent started, to drop its text when it is sent again
    let mut attempt = (meta.lines.len(), None::<usize>);
    let error = loop {
        let Ok(ev) = worker.rx.recv() else { break Some("the chat worker stopped".to_string()) };
        match ev {
            FromWorker::Text(t) => {
                emit(json!({"type": "text", "text": t}));
                match meta.lines.last_mut() {
                    Some(Line::Assistant { text }) => text.push_str(&t),
                    _ => meta.lines.push(Line::Assistant { text: t }),
                }
            }
            FromWorker::Thinking(t) => {
                emit(json!({"type": "thinking", "text": t}));
                match meta.lines.last_mut() {
                    Some(Line::Thinking { text }) => text.push_str(&t),
                    _ => meta.lines.push(Line::Thinking { text: t }),
                }
            }
            FromWorker::ToolStart { name, detail } => {
                emit(json!({"type": "tool_start", "name": name, "detail": detail}));
                meta.lines.push(Line::Tool { name, detail, ok: None, summary: String::new() });
            }
            FromWorker::ToolDone { ok, summary, image } => {
                let mut v = json!({"type": "tool_done", "ok": ok, "summary": summary});
                if let Some(png) = image {
                    v["image"] = json!(base64::engine::general_purpose::STANDARD.encode(png));
                }
                emit(v);
                if let Some(Line::Tool { ok: o, summary: s, .. }) = meta.lines.iter_mut().rev().find(|l| matches!(l, Line::Tool { ok: None, .. })) {
                    *o = Some(ok);
                    *s = summary;
                }
            }
            FromWorker::Workspace { json, .. } => changed = Some(json),
            FromWorker::Notice(t) => {
                emit(json!({"type": "notice", "text": t}));
                meta.lines.push(Line::Notice { text: t, error: false });
            }
            FromWorker::Attempt => {
                emit(json!({"type": "attempt"}));
                let last = match meta.lines.last() {
                    Some(Line::Assistant { text } | Line::Thinking { text }) => Some(text.len()),
                    _ => None,
                };
                attempt = (meta.lines.len(), last);
            }
            FromWorker::Rewind => {
                emit(json!({"type": "rewind"}));
                let (n, last) = attempt;
                meta.lines.truncate(n);
                if let (Some(len), Some(Line::Assistant { text } | Line::Thinking { text })) = (last, meta.lines.last_mut()) {
                    text.truncate(len);
                }
            }
            FromWorker::State { messages, cc_session, planned } => {
                ctx.messages = messages;
                ctx.claude_code_session = cc_session;
                ctx.planned = planned;
            }
            FromWorker::Done { error } => break error,
        }
    };
    if let Some(e) = &error {
        meta.lines.push(Line::Notice { text: e.clone(), error: true });
    }
    // Claude's changes go into the document file (also those made before a stop or an error)
    let mut code = if error.is_some() { 1 } else { 0 };
    let mut saved = false;
    if let Some(json) = changed.filter(|j| *j != ws_text) {
        match crate::session::write_atomic(&doc, json.as_bytes()) {
            Ok(()) => {
                saved = true;
                emit(json!({"type": "saved", "path": doc.display().to_string()}))
            }
            Err(e) => {
                emit(json!({"type": "notice", "text": format!("Could not save {}: {e}", doc.display())}));
                code = 1;
            }
        }
    }
    if let Err(e) = claude_chats::save(&doc, &mut meta, &ctx) {
        emit(json!({"type": "notice", "text": format!("Could not save this chat: {e}")}));
    }
    if saved {
        // as the editor does after a save: commit (when the project commits on save), then sync
        let ask: String = a.prompt.lines().next().unwrap_or("").chars().take(100).collect();
        let r = crate::github_cli::commit_and_sync(&doc, Some(&format!("From a Claude chat on the rCAD phone app: {ask}")));
        let mut parts = vec![];
        if let Some(c) = r["committed"].as_str() {
            parts.push(format!("Committed “{c}”"));
        }
        if let Some(s) = r["synced"].as_str() {
            parts.push(s.to_string());
        }
        if !r["diverged"].is_null() {
            parts.push("GitHub has other changes to this project that can't be combined with these: sync it from the menu to pick one".into());
        }
        if let Some(e) = r["error"].as_str() {
            parts.push(format!("Could not {}", e.trim_start_matches("could not ")));
        }
        if !parts.is_empty() {
            emit(json!({"type": "notice", "text": parts.join(" · "), "git": r}));
        }
    }
    emit(json!({"type": "done", "error": error}));
    code
}
