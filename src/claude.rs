//! Design in natural language: Claude drives the same tools as the MCP server
//! (`agent.rs`) on a copy of the open document, and every change it makes is
//! copied back into the editor.
//!
//! Requests go to the Claude Messages API, authenticated with an API key (kept
//! in the user's config folder) or a Claude Console sign-in made with the `ant`
//! CLI. Runs on a background thread; the UI side is `claude_ui.rs`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MODELS: &[(&str, &str)] =
    &[("claude-opus-5-5", "Claude Opus 5.5"), ("claude-sonnet-5-5", "Claude Sonnet 5.5"), ("claude-haiku-5-5", "Claude Haiku 5.5"), ("claude-fable-5-1", "Claude Fable 5.1")];
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

const API_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2026-10-21";
/// Tools that would replace or write the document behind the editor's back.
const HIDDEN_TOOLS: &[&str] = &["new_document", "open_document", "save_document"];
/// Model round trips allowed for one request before Claude has to stop and report.
const MAX_STEPS: usize = 80;
/// Longest tool result text sent back (describe and list_geometry can be large).
const MAX_RESULT: usize = 60_000;

// ---------------------------------------------------------------- settings

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    #[default]
    ApiKey,
    /// A Claude Console account signed in with `ant auth login`.
    Console,
    /// The Claude Code CLI on this computer, with its own login (a Claude plan or a key).
    ClaudeCode,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub auth: Auth,
    pub api_key: String,
    pub model: String,
    pub effort: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { auth: Auth::ApiKey, api_key: String::new(), model: MODELS[0].0.into(), effort: "high".into() }
    }
}

impl Settings {
    /// The stored key, or `ANTHROPIC_API_KEY` from the environment.
    pub fn key(&self) -> Option<String> {
        let k = self.api_key.trim();
        if !k.is_empty() {
            return Some(k.to_string());
        }
        std::env::var("ANTHROPIC_API_KEY").ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
    }

    pub fn key_from_env(&self) -> bool {
        self.api_key.trim().is_empty() && self.key().is_some()
    }
}

fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
    Some(base.join("cad-rust-app").join("claude.json"))
}

pub fn load_settings() -> Settings {
    settings_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Save the settings (with the API key) readable by the user only.
pub fn save_settings(s: &Settings) -> Result<PathBuf, String> {
    let p = settings_path().ok_or("no config folder (HOME is not set)")?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&p).map_err(|e| e.to_string())?;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    Ok(p)
}

// ---------------------------------------------------------------- credentials

enum Cred {
    Key(String),
    /// Short-lived OAuth access token of a Claude Console sign-in.
    Bearer(String),
}

fn ant(args: &[&str]) -> Result<std::process::Output, String> {
    let mut c = std::process::Command::new("ant");
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "The ant CLI is not installed (see platform.claude.com/docs for the Anthropic CLI), or use an API key instead".to_string()
        } else {
            format!("could not run ant: {e}")
        }
    })
}

fn credentials(s: &Settings) -> Result<Cred, String> {
    match s.auth {
        Auth::ApiKey => s.key().map(Cred::Key).ok_or_else(|| "Add an API key in the Claude settings first".into()),
        Auth::ClaudeCode => Err("Claude Code signs in by itself".into()),
        Auth::Console => {
            // print-credentials refreshes the token when it has expired
            let out = ant(&["auth", "print-credentials", "--access-token"])?;
            let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !out.status.success() || token.is_empty() {
                return Err(format!("Not signed in to a Claude Console account: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            Ok(Cred::Bearer(token))
        }
    }
}

/// `ant auth login`: opens the browser to sign in to a Claude Console account.
pub fn console_login() -> Result<String, String> {
    let out = ant(&["auth", "login"])?;
    if out.status.success() { console_status() } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// Which account and workspace the `ant` sign-in uses.
pub fn console_status() -> Result<String, String> {
    let out = ant(&["auth", "status"])?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
    if out.status.success() { Ok(text) } else { Err(text) }
}

// ---------------------------------------------------------------- API

/// Streamed pieces of Claude's reply, for the chat panel.
enum Delta<'a> {
    Text(&'a str),
    Thinking(&'a str),
}

struct Failure {
    message: String,
    retry: Option<Duration>,
}

fn fail(message: impl Into<String>) -> Failure {
    Failure { message: message.into(), retry: None }
}

/// One streamed Messages API call. Returns the assembled message.
fn stream_message(agent: &ureq::Agent, cred: &Cred, body: &Value, betas: &[&str], stop: &AtomicBool, on: &mut dyn FnMut(Delta)) -> Result<Value, Failure> {
    let base = std::env::var("ANTHROPIC_BASE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| API_URL.into());
    let mut req = agent.post(&format!("{}/v1/messages", base.trim_end_matches('/'))).set("content-type", "application/json").set("anthropic-version", API_VERSION);
    let mut betas = betas.to_vec();
    req = match cred {
        Cred::Key(k) => req.set("x-api-key", k),
        Cred::Bearer(t) => {
            betas.push("oauth-2025-04-20");
            req.set("authorization", &format!("Bearer {t}"))
        }
    };
    if !betas.is_empty() {
        req = req.set("anthropic-beta", &betas.join(","));
    }
    let resp = match req.send_string(&body.to_string()) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let retry = r.header("retry-after").and_then(|s| s.parse::<u64>().ok()).map(Duration::from_secs);
            let text = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or(text);
            let message = match code {
                401 => format!("The API key or sign-in was rejected ({msg})"),
                403 => format!("Not allowed: {msg}"),
                429 => format!("Rate limited: {msg}"),
                _ => format!("Claude API error {code}: {msg}"),
            };
            let retry = if code == 429 || code == 529 || code >= 500 { Some(retry.unwrap_or(Duration::from_secs(5))) } else { None };
            return Err(Failure { message, retry });
        }
        Err(e) => return Err(Failure { message: format!("Could not reach the Claude API: {e}"), retry: Some(Duration::from_secs(3)) }),
    };

    let mut msg = json!({});
    let mut blocks: Vec<Value> = vec![];
    // tool inputs arrive as JSON text in pieces
    let mut partial: Vec<String> = vec![];
    for line in BufReader::new(resp.into_reader()).lines() {
        if stop.load(Ordering::Relaxed) {
            return Err(fail("Stopped"));
        }
        let line = line.map_err(|e| Failure { message: format!("The connection dropped: {e}"), retry: None })?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
        let i = ev["index"].as_u64().unwrap_or(0) as usize;
        match ev["type"].as_str().unwrap_or("") {
            "message_start" => msg = ev["message"].clone(),
            "content_block_start" => {
                while blocks.len() <= i {
                    blocks.push(Value::Null);
                    partial.push(String::new());
                }
                blocks[i] = ev["content_block"].clone();
            }
            "content_block_delta" => {
                let Some(b) = blocks.get_mut(i) else { continue };
                let d = &ev["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        b["text"] = json!(format!("{}{t}", b["text"].as_str().unwrap_or("")));
                        on(Delta::Text(t));
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or("");
                        b["thinking"] = json!(format!("{}{t}", b["thinking"].as_str().unwrap_or("")));
                        on(Delta::Thinking(t));
                    }
                    "signature_delta" => b["signature"] = d["signature"].clone(),
                    "input_json_delta" => partial[i].push_str(d["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(b) = blocks.get_mut(i) {
                    if b["type"] == "tool_use" && !partial[i].trim().is_empty() {
                        match serde_json::from_str::<Value>(&partial[i]) {
                            Ok(v) => b["input"] = v,
                            // answered with an error result in run_tools
                            Err(_) => b["input"] = json!({"__invalid_json": partial[i].clone()}),
                        }
                    }
                }
            }
            "message_delta" => {
                for (k, v) in ev["delta"].as_object().into_iter().flatten() {
                    msg[k] = v.clone();
                }
                if let Some(u) = ev["usage"].as_object() {
                    for (k, v) in u {
                        msg["usage"][k] = v.clone();
                    }
                }
            }
            "error" => {
                let kind = ev["error"]["type"].as_str().unwrap_or("");
                let retry = matches!(kind, "overloaded_error" | "api_error").then(|| Duration::from_secs(5));
                return Err(Failure { message: format!("Claude API error: {}", ev["error"]["message"].as_str().unwrap_or(kind)), retry });
            }
            _ => {}
        }
    }
    msg["content"] = Value::Array(blocks.into_iter().filter(|b| !b.is_null()).collect());
    Ok(msg)
}

// ---------------------------------------------------------------- worker

pub enum ToWorker {
    Turn {
        text: String,
        context: String,
        ws: String,
        studio: usize,
        settings: Settings,
    },
    /// Forget the conversation.
    Reset,
}

pub enum FromWorker {
    Text(String),
    Thinking(String),
    ToolStart {
        name: String,
        detail: String,
    },
    ToolDone {
        ok: bool,
        summary: String,
        image: Option<Vec<u8>>,
    },
    /// The document changed: the whole workspace and its active Part Studio.
    Workspace {
        json: String,
        studio: usize,
    },
    Notice(String),
    Done {
        error: Option<String>,
    },
}

pub struct Worker {
    pub tx: Sender<ToWorker>,
    pub rx: Receiver<FromWorker>,
    pub stop: Arc<AtomicBool>,
}

impl Worker {
    pub fn start(ctx: egui::Context) -> Worker {
        let (tx, wrx) = channel::<ToWorker>();
        let (wtx, rx) = channel::<FromWorker>();
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        std::thread::Builder::new()
            .name("claude".into())
            .spawn(move || {
                let send = |e: FromWorker| {
                    let _ = wtx.send(e);
                    ctx.request_repaint();
                };
                let mut chat = Chat::new();
                while let Ok(m) = wrx.recv() {
                    match m {
                        ToWorker::Reset => {
                            chat.messages.clear();
                            chat.cc_session = None;
                        }
                        ToWorker::Turn { text, context, ws, studio, settings } => {
                            st.store(false, Ordering::Relaxed);
                            let error = chat.turn(&text, &context, &ws, studio, &settings, &st, &send).err();
                            send(FromWorker::Done { error });
                        }
                    }
                }
            })
            .expect("spawn claude thread");
        Worker { tx, rx, stop }
    }
}

struct Chat {
    session: Option<crate::agent::Session>,
    /// The document as the session last saw it, to skip reloading an unchanged one.
    seen: String,
    messages: Vec<Value>,
    tools: Value,
    http: ureq::Agent,
    /// Claude Code: the conversation to resume, and where its MCP server reaches us.
    cc_session: Option<String>,
    bridge: Option<Bridge>,
}

impl Chat {
    fn new() -> Chat {
        let tools: Vec<Value> = crate::agent::tools()
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| !HIDDEN_TOOLS.contains(&t["name"].as_str().unwrap_or("")))
            .map(|t| json!({"name": t["name"], "description": t["description"], "input_schema": t["inputSchema"], "eager_input_streaming": true}))
            .collect();
        let http = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(30)).timeout_read(Duration::from_secs(600)).try_proxy_from_env(true).build();
        Chat { session: None, seen: String::new(), messages: vec![], tools: Value::Array(tools), http, cc_session: None, bridge: None }
    }

    fn turn(&mut self, text: &str, context: &str, ws: &str, studio: usize, settings: &Settings, stop: &AtomicBool, send: &dyn Fn(FromWorker)) -> Result<(), String> {
        let cred = if settings.auth == Auth::ClaudeCode { None } else { Some(credentials(settings)?) };
        // start from what is open in the editor
        let session = self.session.get_or_insert_with(crate::agent::Session::new);
        if ws != self.seen {
            let w = crate::workspace::Workspace::from_json(ws)?;
            session.load_workspace(w, studio)?;
            self.seen = session.workspace_json().0;
        }
        let prompt = format!("<editor>\n{context}\n</editor>\n\n{text}");
        let Some(cred) = cred else { return self.claude_code_turn(&prompt, settings, stop, send) };
        let user = json!({"type": "text", "text": prompt});
        // after a stopped or failed turn the history already ends with the user's side
        match self.messages.last_mut() {
            Some(m) if m["role"] == "user" => m["content"].as_array_mut().unwrap().push(user),
            _ => self.messages.push(json!({"role": "user", "content": [user]})),
        }

        let mut betas = vec![];
        let mut body = json!({
            "model": settings.model,
            "max_tokens": 64000,
            "stream": true,
            "system": system_prompt(),
            "tools": self.tools,
            "thinking": {"type": "adaptive", "display": "summarized"},
            "output_config": {"effort": settings.effort},
            "cache_control": {"type": "ephemeral"},
        });
        if settings.model != "claude-haiku-5-5" {
            // a safety classifier's false positive is answered by another model instead of a refusal
            body["fallbacks"] = json!("default");
            betas.push("server-side-fallback-2026-07-01");
        }

        for _ in 0..MAX_STEPS {
            if stop.load(Ordering::Relaxed) {
                return Err("Stopped".into());
            }
            body["messages"] = Value::Array(self.messages.clone());
            let mut tries = 0;
            let msg = loop {
                let mut on = |d: Delta| match d {
                    Delta::Text(t) => send(FromWorker::Text(t.into())),
                    Delta::Thinking(t) => send(FromWorker::Thinking(t.into())),
                };
                match stream_message(&self.http, &cred, &body, &betas, stop, &mut on) {
                    Ok(m) => break m,
                    Err(Failure { message, retry: Some(wait) }) if tries < 3 && !stop.load(Ordering::Relaxed) => {
                        tries += 1;
                        send(FromWorker::Notice(format!("{message}; retrying in {} s", wait.as_secs())));
                        std::thread::sleep(wait.min(Duration::from_secs(60)));
                    }
                    Err(f) => return Err(f.message),
                }
            };
            let stop_reason = msg["stop_reason"].as_str().unwrap_or("").to_string();
            if stop_reason == "refusal" {
                let cat = msg["stop_details"]["category"].as_str().map(|c| format!(" ({c})")).unwrap_or_default();
                return Err(format!("Claude declined this request{cat}. Try rephrasing it."));
            }
            let content = msg["content"].as_array().cloned().unwrap_or_default();
            if content.is_empty() {
                return Ok(());
            }
            // the reply goes back unchanged, thinking blocks and all
            self.messages.push(json!({"role": "assistant", "content": content}));
            let calls: Vec<Value> = content.iter().filter(|b| b["type"] == "tool_use").cloned().collect();
            if calls.is_empty() {
                if stop_reason == "max_tokens" {
                    send(FromWorker::Notice("The reply hit the length limit.".into()));
                }
                return Ok(());
            }
            let results = self.run_tools(&calls, stop, send);
            self.messages.push(json!({"role": "user", "content": results}));
            if stop_reason == "max_tokens" {
                send(FromWorker::Notice("The reply hit the length limit.".into()));
                return Ok(());
            }
        }
        send(FromWorker::Notice(format!("Stopped after {MAX_STEPS} steps; say \"continue\" to go on.")));
        Ok(())
    }

    /// Run Claude's tool calls on the session, sending changed documents to the editor.
    fn run_tools(&mut self, calls: &[Value], stop: &AtomicBool, send: &dyn Fn(FromWorker)) -> Vec<Value> {
        let mut results = vec![];
        for c in calls {
            let id = c["id"].clone();
            if stop.load(Ordering::Relaxed) {
                results.push(json!({"type": "tool_result", "tool_use_id": id, "content": "Stopped by the user before this ran.", "is_error": true}));
                continue;
            }
            let (text, png, failed) = self.run_tool(c["name"].as_str().unwrap_or(""), &c["input"], send);
            let mut content = vec![json!({"type": "text", "text": text})];
            if let Some(png) = &png {
                content.push(json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png)}}));
            }
            results.push(json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": failed}));
        }
        results
    }

    /// Run one tool on the session: result text, an optional PNG, and whether it failed.
    fn run_tool(&mut self, name: &str, input: &Value, send: &dyn Fn(FromWorker)) -> (String, Option<Vec<u8>>, bool) {
        send(FromWorker::ToolStart { name: name.into(), detail: short(&input.to_string(), 160) });
        if input.get("__invalid_json").is_some() || !input.is_object() {
            send(FromWorker::ToolDone { ok: false, summary: "invalid input".into(), image: None });
            return ("The tool input was not valid JSON; send the call again.".into(), None, true);
        }
        if HIDDEN_TOOLS.contains(&name) {
            send(FromWorker::ToolDone { ok: false, summary: "not available here".into(), image: None });
            return (format!("{name} is not available in the editor; the user opens and saves documents themselves."), None, true);
        }
        let session = self.session.as_mut().unwrap();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.call(name, input))).unwrap_or_else(|_| Err(format!("{name} crashed")));
        let out = match r {
            Ok((v, img)) => {
                let mut text = v.to_string();
                if text.len() > MAX_RESULT {
                    let mut cut = MAX_RESULT;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    text.push_str(" …(truncated; ask for less, e.g. filter list_geometry)");
                }
                send(FromWorker::ToolDone { ok: true, summary: summarize(&v), image: img.clone() });
                (text, img, false)
            }
            Err(e) => {
                send(FromWorker::ToolDone { ok: false, summary: short(&e, 200), image: None });
                (e, None, true)
            }
        };
        let (ws, studio) = session.workspace_json();
        if ws != self.seen {
            self.seen = ws.clone();
            send(FromWorker::Workspace { json: ws, studio });
        }
        out
    }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// A line for the chat panel about a tool result.
fn summarize(v: &Value) -> String {
    let mut out = vec![];
    if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
        out.push(format!("{} feature error(s)", errs.len()));
    }
    if let Some(parts) = v["parts"].as_array() {
        out.push(format!("{} part(s)", parts.len()));
    }
    for k in ["sketch_id", "feature_id", "id", "saved", "exported"] {
        if let Some(x) = v.get(k).filter(|x| !x.is_null()) {
            out.push(format!("{k} {x}"));
        }
    }
    if out.is_empty() { "done".into() } else { out.join(", ") }
}

fn system_prompt() -> Value {
    json!([{"type": "text", "text": system_text()}])
}

fn system_text() -> String {
    format!(
        "You are the design assistant built into cad-app, a parametric CAD program (OpenCascade kernel, Onshape-style Part Studios, Assemblies, Drawings, Manufacture and Simulation tabs). \
The user types requests in a chat panel beside the 3D view, and you carry them out with the tools, which edit the document the user has open. Every change shows up in their editor as you make it, and they can undo your whole turn from the chat panel.\n\n\
How to work:\n\
- The document is already open. Call describe when you need to know what is in it. Each message starts with an <editor> block saying which tab is open, what the user has selected (refs you can use directly, e.g. for \"fillet these edges\") and where the document lives.\n\
- Build parametric models the user can edit later: variables for the key sizes, constrained and dimensioned sketches, features that reference sketches and refs from list_geometry. Never guess refs.\n\
- After changing geometry, render it and look at the image to check the result before you finish. Fix feature errors you caused.\n\
- If a request leaves a design choice open, pick something sensible, say what you assumed, and carry on. Ask only when you can't make progress.\n\
- Write exported files next to the document (or in the home folder if it is unsaved) with absolute paths, and say where they went.\n\
- Keep replies short and plain: what you made, the key dimensions, any assumptions. The chat panel shows plain text, so don't use Markdown tables or headings.\n\n\
cad-app's agent guide, for reference:\n\n{}",
        include_str!("../AGENTS.md")
    )
}

// ---------------------------------------------------------------- Claude Code

/// A tool call from the `cad-app mcp --bridge` process that Claude Code runs.
struct BridgeCall {
    name: String,
    args: Value,
    reply: Sender<Value>,
}

/// A local socket the bridge process sends tool calls to, guarded by a random token.
struct Bridge {
    addr: String,
    token: String,
    rx: Receiver<BridgeCall>,
}

impl Bridge {
    fn start() -> Result<Bridge, String> {
        use std::hash::{BuildHasher, Hasher};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("could not open a local port for Claude Code: {e}"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?.to_string();
        let token: String = (0..2)
            .map(|i| {
                let mut h = std::collections::hash_map::RandomState::new().build_hasher();
                h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() + i);
                format!("{:016x}", h.finish())
            })
            .collect();
        let (tx, rx) = channel::<BridgeCall>();
        let tok = token.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (tx, tok) = (tx.clone(), tok.clone());
                std::thread::spawn(move || {
                    let Ok(mut out) = conn.try_clone() else { return };
                    for line in BufReader::new(conn).lines() {
                        let Ok(line) = line else { return };
                        let Ok(req) = serde_json::from_str::<Value>(&line) else { return };
                        if req["token"].as_str() != Some(tok.as_str()) {
                            return;
                        }
                        let (rtx, rrx) = channel();
                        let call = BridgeCall { name: req["name"].as_str().unwrap_or("").into(), args: req["arguments"].clone(), reply: rtx };
                        if tx.send(call).is_err() {
                            return;
                        }
                        // no reply means no request is running: Claude Code called after a stop
                        let reply = rrx.recv().unwrap_or_else(|_| json!({"content": [{"type": "text", "text": "The cad-app chat is not running a request."}], "isError": true}));
                        use std::io::Write;
                        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Ok(Bridge { addr, token, rx })
    }
}

/// Lines Claude Code prints, read on their own thread.
enum CcLine {
    Line(String),
    End,
}

fn claude_command() -> std::process::Command {
    let name = std::env::var("CLAUDE_CODE_PATH").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| "claude".into());
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(name);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c
}

/// `claude --version`, to check Claude Code is installed.
pub fn claude_code_version() -> Result<String, String> {
    let out = claude_command().arg("--version").output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "Claude Code is not installed or not on PATH (set CLAUDE_CODE_PATH to its full path)".to_string()
        } else {
            format!("could not run claude: {e}")
        }
    })?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() { Ok(format!("Found Claude Code {v}")) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

impl Chat {
    /// One request through the Claude Code CLI (`claude -p`), which reaches the open document
    /// through `cad-app mcp --bridge` and this worker. Follow-ups resume the same conversation.
    fn claude_code_turn(&mut self, prompt: &str, settings: &Settings, stop: &AtomicBool, send: &dyn Fn(FromWorker)) -> Result<(), String> {
        let bridge = match self.bridge.take() {
            Some(b) => b,
            None => Bridge::start()?,
        };
        let r = self.claude_code_run(&bridge, prompt, settings, stop, send);
        self.bridge = Some(bridge);
        r
    }

    fn claude_code_run(&mut self, bridge: &Bridge, prompt: &str, settings: &Settings, stop: &AtomicBool, send: &dyn Fn(FromWorker)) -> Result<(), String> {
        let (addr, token) = (&bridge.addr, &bridge.token);
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mcp = json!({"mcpServers": {"cad": {"command": exe, "args": ["mcp", "--bridge", addr, "--token", token]}}});
        let mut cmd = claude_command();
        cmd.args(["-p", prompt, "--output-format", "stream-json", "--verbose", "--strict-mcp-config", "--tools", "", "--allowedTools", "mcp__cad", "--permission-mode", "dontAsk"])
            .arg("--mcp-config")
            .arg(mcp.to_string())
            .arg("--append-system-prompt")
            .arg(format!("{}\n\nThe tools are the cad MCP server's (mcp__cad__describe and so on).", system_text()))
            .args(["--model", &settings.model, "--effort", &settings.effort]);
        if let Some(id) = &self.cc_session {
            cmd.args(["--resume", id]);
        }
        // a neutral folder, so no project's CLAUDE.md or settings come along
        let dir = std::env::temp_dir().join("cad-app-claude");
        let _ = std::fs::create_dir_all(&dir);
        cmd.current_dir(&dir).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Claude Code is not installed or not on PATH (set CLAUDE_CODE_PATH to its full path)".to_string()
            } else {
                format!("could not start Claude Code: {e}")
            }
        })?;
        let (ltx, lrx) = channel::<CcLine>();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = ltx.send(CcLine::Line(line));
            }
            let _ = ltx.send(CcLine::End);
        });
        let mut stderr = child.stderr.take().unwrap();
        let err_text = std::thread::spawn(move || {
            let mut t = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut t);
            t
        });

        let mut error = None;
        let mut finished = false;
        loop {
            if stop.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Stopped".into());
            }
            while let Ok(c) = bridge.rx.try_recv() {
                let (text, png, failed) = self.run_tool(&c.name, &c.args, send);
                let mut content = vec![json!({"type": "text", "text": text})];
                if let Some(png) = png {
                    content.push(json!({"type": "image", "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png)}));
                }
                let _ = c.reply.send(json!({"content": content, "isError": failed}));
            }
            match lrx.recv_timeout(Duration::from_millis(30)) {
                Ok(CcLine::Line(l)) => {
                    let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                    if let Some(id) = v["session_id"].as_str() {
                        self.cc_session = Some(id.to_string());
                    }
                    match v["type"].as_str().unwrap_or("") {
                        "system" if v["subtype"] == "init" => {
                            let cad = v["mcp_servers"].as_array().and_then(|a| a.iter().find(|m| m["name"] == "cad").cloned());
                            if let Some(st) = cad.as_ref().and_then(|m| m["status"].as_str()).filter(|st| *st != "connected" && *st != "pending") {
                                send(FromWorker::Notice(format!("Claude Code could not start the cad tools ({st})")));
                            }
                        }
                        "assistant" => {
                            for b in v["message"]["content"].as_array().into_iter().flatten() {
                                match b["type"].as_str().unwrap_or("") {
                                    "text" => send(FromWorker::Text(format!("{}\n", b["text"].as_str().unwrap_or("")))),
                                    "thinking" if !b["thinking"].as_str().unwrap_or("").is_empty() => send(FromWorker::Thinking(b["thinking"].as_str().unwrap_or("").into())),
                                    _ => {}
                                }
                            }
                        }
                        "result" => {
                            finished = true;
                            if v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success") {
                                let why = v["result"].as_str().or(v["subtype"].as_str()).unwrap_or("unknown error");
                                error = Some(format!("Claude Code: {why}"));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(CcLine::End) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        let err_text = err_text.join().unwrap_or_default();
        if let Some(e) = error {
            return Err(e);
        }
        if !finished || !status.success() {
            let tail: String = err_text.trim().lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            // a resumed conversation that is gone: start a fresh one next time
            self.cc_session = None;
            return Err(format!("Claude Code stopped ({status}){}", if tail.is_empty() { String::new() } else { format!(": {tail}") }));
        }
        Ok(())
    }
}
