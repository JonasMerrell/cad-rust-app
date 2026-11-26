//! The chat panel through the Gemini CLI, the Codex CLI or the Antigravity CLI instead of Claude:
//! each runs on this computer with its own sign-in (a Google account with a Gemini plan, a
//! ChatGPT plan, or a Google account for Antigravity), and
//! reaches the open document through `rcad mcp --bridge-from-env` and this worker, the same way
//! the Claude Code mode does.
//!
//! Both CLIs keep their tool settings in files rather than taking them on the command line:
//! - Gemini: a run folder in rCAD's config folder (`gemini-run/`) with `.gemini/settings.json`
//!   (the cad MCP server), `GEMINI.md` (rCAD's instructions) and `policy.toml`, which turns every
//!   other tool off (no shell, files or web). The folder is the same every time, so the Gemini CLI
//!   finds its saved conversations again.
//! - Codex: a profile, `rcad.config.toml` in the Codex home folder (`~/.codex`), with the cad MCP
//!   server, rCAD's instructions, a read-only sandbox, no shell and no web search.
//! - Antigravity (`agy`): a run folder (`agy-run/`) with `.agents/mcp_config.json` (the cad MCP
//!   server) and `AGENTS.md` (rCAD's instructions); the permissions go per run on the command
//!   line (`--settings`): the cad tools allowed, commands, file reads and writes and web pages
//!   denied, and the default mode (not auto, which approves commands by itself headless). Its
//!   built-in web search has no rule and stays.
//!
//! The bridge address and token go to the MCP server through the environment, never through a
//! file or the command line.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cli {
    Gemini,
    Codex,
    Antigravity,
}

/// What a line of a CLI's JSON output says.
#[derive(Debug, PartialEq)]
pub(super) enum CliEvent {
    /// The conversation's id, to resume it next time.
    Session(String),
    Text(String),
    Thinking(String),
    Notice(String),
    /// The request is over, with the error if it failed.
    Finished(Option<String>),
}

impl Cli {
    pub fn of(auth: Auth) -> Option<Cli> {
        match auth {
            Auth::Gemini => Some(Cli::Gemini),
            Auth::Codex => Some(Cli::Codex),
            Auth::Antigravity => Some(Cli::Antigravity),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Cli::Gemini => "Gemini CLI",
            Cli::Codex => "Codex",
            Cli::Antigravity => "Antigravity CLI",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Cli::Gemini => "gemini",
            Cli::Codex => "codex",
            Cli::Antigravity => "agy",
        }
    }

    /// The environment variable that names the program's full path.
    pub fn path_env(self) -> &'static str {
        match self {
            Cli::Gemini => "GEMINI_CLI_PATH",
            Cli::Codex => "CODEX_PATH",
            Cli::Antigravity => "AGY_PATH",
        }
    }

    /// Saved conversation ids start with this, so a chat knows which program its id belongs to
    /// (Claude Code ids have none).
    pub fn prefix(self) -> &'static str {
        match self {
            Cli::Gemini => "gemini:",
            Cli::Codex => "codex:",
            Cli::Antigravity => "agy:",
        }
    }

    fn install_hint(self) -> String {
        let how = match self {
            Cli::Gemini => "npm install -g @google/gemini-cli, then run gemini once to sign in with your Google account",
            Cli::Codex => "npm install -g @openai/codex, then run codex login to sign in with ChatGPT",
            Cli::Antigravity => "curl -fsSL https://antigravity.google/cli/install.sh | bash, then run agy once to sign in with your Google account",
        };
        format!("The {} is not installed or not on PATH ({how}; or set {} to its full path)", self.label(), self.path_env())
    }
}

/// Which program a saved conversation id belongs to: None for Claude Code.
pub(super) fn session_cli(id: &str) -> Option<Cli> {
    [Cli::Gemini, Cli::Codex, Cli::Antigravity].into_iter().find(|c| id.starts_with(c.prefix()))
}

/// The program, by its path variable or on PATH. On Windows npm installs these as `.cmd` scripts,
/// which have to be named in full.
fn command(cli: Cli) -> std::process::Command {
    let name = std::env::var(cli.path_env()).ok().filter(|p| !p.is_empty()).unwrap_or_else(|| find_on_path(cli.program()));
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(name);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c
}

fn find_on_path(program: &str) -> String {
    if cfg!(windows) {
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                for ext in ["exe", "cmd", "bat"] {
                    let p = dir.join(format!("{program}.{ext}"));
                    if p.is_file() {
                        return p.to_string_lossy().into_owned();
                    }
                }
            }
        }
    }
    program.to_string()
}

/// `gemini --version` / `codex --version`, to check the program is installed.
pub fn version(cli: Cli) -> Result<String, String> {
    let out = command(cli).arg("--version").stdin(std::process::Stdio::null()).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { cli.install_hint() } else { format!("could not run {}: {e}", cli.program()) }
    })?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let v = v.trim_start_matches("codex-cli ").to_string();
    let signed_in = match cli {
        // `codex login status` says how Codex is signed in
        Cli::Codex => command(cli)
            .args(["login", "status"])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().lines().next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .map(|s| format!(" · {s}"))
            .unwrap_or_default(),
        Cli::Gemini | Cli::Antigravity => String::new(),
    };
    Ok(format!("Found {} {v}{signed_in}", cli.label()))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()).or_else(|| home().map(|h| h.join(".codex")))
}

/// Whether the program still has the conversation `id` (without its prefix) to resume. When its
/// folder can't be read, say yes and let the program decide.
pub(super) fn has_session(cli: Cli, id: &str) -> bool {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return false;
    }
    match cli {
        // ~/.gemini/tmp/<project>/chats/session-<time>-<first 8 of the id>.jsonl
        Cli::Gemini => {
            let Some(Ok(dirs)) = home().map(|h| std::fs::read_dir(h.join(".gemini").join("tmp"))) else { return true };
            let short: String = id.chars().take(8).collect();
            dirs.filter_map(|d| d.ok()).any(|d| {
                std::fs::read_dir(d.path().join("chats")).map(|f| f.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().contains(&short))).unwrap_or(false)
            })
        }
        // ~/.codex/sessions/<year>/<month>/<day>/rollout-<time>-<id>.jsonl
        Cli::Codex => {
            let Some(root) = codex_home().map(|h| h.join("sessions")) else { return true };
            if !root.is_dir() {
                return true;
            }
            fn walk(dir: &std::path::Path, id: &str, depth: usize) -> bool {
                let Ok(entries) = std::fs::read_dir(dir) else { return false };
                entries.filter_map(|e| e.ok()).any(|e| {
                    let p = e.path();
                    if p.is_dir() { depth < 4 && walk(&p, id, depth + 1) } else { e.file_name().to_string_lossy().contains(id) }
                })
            }
            walk(&root, id, 0)
        }
        // ~/.gemini/antigravity-cli/conversations/<id>.db
        Cli::Antigravity => {
            let Some(dir) = home().map(|h| h.join(".gemini").join("antigravity-cli").join("conversations")) else { return true };
            !dir.is_dir() || dir.join(format!("{id}.db")).is_file()
        }
    }
}

fn instructions() -> String {
    format!(
        "{}\n\nYou are running inside rCAD's chat panel. The only tools are the cad MCP server's (describe, sketch_add, add_feature, render and so on); there is no shell, file or web access, \
and none is needed: everything goes through the cad tools, which edit the document the user has open.",
        system_text()
    )
}

/// A TOML string (JSON's string escapes are valid TOML basic-string escapes).
fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The run folder for the Gemini CLI, with its settings written. It stays the same from run to
/// run (the Gemini CLI keeps conversations per folder), in rCAD's own config folder, which only
/// the user can write to.
fn gemini_folder(exe: &std::path::Path) -> Result<PathBuf, String> {
    let base = settings_path().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or("no config folder (HOME is not set)")?;
    let dir = base.join("gemini-run");
    std::fs::create_dir_all(dir.join(".gemini")).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let env = json!({(crate::agent::BRIDGE_TOKEN_ENV): format!("${}", crate::agent::BRIDGE_TOKEN_ENV), (crate::agent::BRIDGE_ADDR_ENV): format!("${}", crate::agent::BRIDGE_ADDR_ENV)});
    let settings = json!({
        "mcpServers": {"cad": {"command": exe, "args": ["mcp", "--bridge-from-env"], "env": env, "trust": true, "timeout": 900_000}},
    });
    let policy = "# Written by rCAD: in its chat panel the Gemini CLI may only use rCAD's cad tools.\n\n\
[[rule]]\ntoolName = \"*\"\ndecision = \"deny\"\npriority = 1\n\n\
[[rule]]\nmcpName = \"cad\"\ntoolName = \"*\"\ndecision = \"allow\"\npriority = 900\n";
    let write = |p: PathBuf, t: &str| write_private(&p, t).map_err(|e| format!("could not write {}: {e}", p.display()));
    write(dir.join(".gemini").join("settings.json"), &serde_json::to_string_pretty(&settings).unwrap_or_default())?;
    write(dir.join("GEMINI.md"), &instructions())?;
    write(dir.join("policy.toml"), policy)?;
    Ok(dir)
}

/// The run folder for the Antigravity CLI, with the cad MCP server and rCAD's instructions.
fn agy_folder(exe: &std::path::Path) -> Result<PathBuf, String> {
    let base = settings_path().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or("no config folder (HOME is not set)")?;
    let dir = base.join("agy-run");
    std::fs::create_dir_all(dir.join(".agents")).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    // the server inherits agy's environment, which has the bridge's address and token
    let mcp = json!({"mcpServers": {"cad": {"command": exe, "args": ["mcp", "--bridge-from-env"], "timeoutSeconds": 900}}});
    let write = |p: PathBuf, t: &str| write_private(&p, t).map_err(|e| format!("could not write {}: {e}", p.display()));
    write(dir.join(".agents").join("mcp_config.json"), &serde_json::to_string_pretty(&mcp).unwrap_or_default())?;
    write(dir.join("AGENTS.md"), &instructions())?;
    Ok(dir)
}

/// The Antigravity CLI's settings for one run: only the cad tools, and the default mode.
fn agy_settings() -> String {
    json!({
        "agentMode": "",
        "permissions": {
            "allow": ["mcp(cad/*)"],
            "deny": ["command(*)", "unsandboxed(*)", "read_file(*)", "write_file(*)", "read_url(*)", "execute_url(*)", "download_url(*)"],
        },
    })
    .to_string()
}

/// Codex's `rcad` profile, written each run (the rCAD program may have moved).
fn codex_profile(exe: &std::path::Path) -> Result<(), String> {
    let home = codex_home().ok_or("no home folder for Codex (set CODEX_HOME)")?;
    std::fs::create_dir_all(&home).map_err(|e| format!("could not make {}: {e}", home.display()))?;
    let text = format!(
        "# Written by rCAD for its chat panel (codex -p rcad). rCAD rewrites it on every request.\n\
sandbox_mode = \"read-only\"\n\
approval_policy = \"never\"\n\
web_search = \"disabled\"\n\
developer_instructions = {}\n\n\
[features]\nshell_tool = false\nunified_exec = false\n\n\
[mcp_servers.cad]\n\
command = {}\n\
args = [\"mcp\", \"--bridge-from-env\"]\n\
env_vars = [{}, {}]\n\
default_tools_approval_mode = \"approve\"\n\
startup_timeout_sec = 30\n\
tool_timeout_sec = 900\n",
        toml_str(&instructions()),
        toml_str(&exe.to_string_lossy()),
        toml_str(crate::agent::BRIDGE_TOKEN_ENV),
        toml_str(crate::agent::BRIDGE_ADDR_ENV),
    );
    let p = home.join("rcad.config.toml");
    write_private(&p, &text).map_err(|e| format!("could not write {}: {e}", p.display()))
}

/// Codex takes low to xhigh.
fn codex_effort(effort: &str) -> &str {
    match effort {
        "low" | "medium" | "high" | "xhigh" => effort,
        "max" => "xhigh",
        _ => "high",
    }
}

/// The command line for one request. `session` is the conversation to resume (without its
/// prefix); the request itself goes in on stdin.
pub(super) fn args(cli: Cli, folder: &std::path::Path, settings: &Settings, session: Option<&str>, images: &[std::path::PathBuf]) -> Vec<String> {
    let mut a: Vec<String> = vec![];
    let model = settings.cli_model(cli).trim().to_string();
    match cli {
        Cli::Gemini => {
            // the request comes from stdin; -p makes it a headless run
            a.extend(["-p", "", "--output-format", "stream-json", "--approval-mode", "yolo", "--allowed-mcp-server-names", "cad"].map(String::from));
            a.push("--policy".into());
            a.push(folder.join("policy.toml").to_string_lossy().into_owned());
            if !model.is_empty() {
                a.extend(["--model".into(), model]);
            }
            if let Some(id) = session {
                a.extend(["--resume".into(), id.into()]);
            }
        }
        Cli::Codex => {
            a.extend(["--profile", "rcad", "exec"].map(String::from));
            if session.is_some() {
                a.push("resume".into());
            }
            a.extend(["--json", "--skip-git-repo-check"].map(String::from));
            if !model.is_empty() {
                a.extend(["--model".into(), model]);
            }
            a.extend(["-c".into(), format!("model_reasoning_effort={}", codex_effort(&settings.effort))]);
            for p in images {
                a.push(format!("--image={}", p.to_string_lossy()));
            }
            a.push("--".into());
            if let Some(id) = session {
                a.push(id.into());
            }
            a.push("-".into());
        }
        Cli::Antigravity => {
            // the request comes in on stdin as one stream-json message (see `stdin_text`)
            a.extend(["--input-format", "stream-json", "--output-format", "stream-json"].map(String::from));
            a.extend(["--settings".into(), agy_settings()]);
            // its model ids carry their effort (gemini-4-neon-high); --effort only goes with the
            // default model (agy refuses both)
            if !model.is_empty() {
                a.extend(["--model".into(), model]);
            } else if matches!(settings.effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max") {
                a.extend(["--effort".into(), settings.effort.clone()]);
            }
            if let Some(id) = session {
                a.extend(["--conversation".into(), id.into()]);
            }
            // print mode with its prompt empty: the prompt is the stdin message
            a.push("-p=".into());
        }
    }
    a
}

/// What goes in on stdin: the request as it is, or (Antigravity) as one stream-json message.
fn stdin_text(cli: Cli, prompt: &str) -> String {
    match cli {
        Cli::Antigravity => format!("{}\n", json!({"event": "user", "message": {"content": prompt}})),
        _ => prompt.to_string(),
    }
}

/// Read one line of the program's JSON output.
pub(super) fn parse(cli: Cli, v: &Value) -> Vec<CliEvent> {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let mut out = vec![];
    match cli {
        // init, message (user/assistant, streamed in pieces), tool_use, tool_result, error, result
        Cli::Gemini => match v["type"].as_str().unwrap_or("") {
            "init" if !s("session_id").is_empty() => out.push(CliEvent::Session(s("session_id"))),
            "message" if v["role"] == "assistant" => {
                let t = s("content");
                if !t.is_empty() {
                    out.push(CliEvent::Text(if v["delta"].as_bool().unwrap_or(false) { t } else { format!("{t}\n") }));
                }
            }
            "error" => {
                let m = v["message"].as_str().or(v["error"]["message"].as_str()).unwrap_or("").to_string();
                if !m.is_empty() {
                    out.push(CliEvent::Notice(format!("Gemini: {m}")));
                }
            }
            "result" => {
                let failed = v["status"].as_str().is_some_and(|st| st != "success");
                let why = v["error"]["message"].as_str().or(v["error"].as_str()).or(v["status"].as_str()).unwrap_or("unknown error");
                out.push(CliEvent::Finished(failed.then(|| format!("Gemini CLI: {why}"))));
            }
            _ => {}
        },
        // thread.started, turn.started, item.started/updated/completed, turn.completed/failed, error
        Cli::Codex => match v["type"].as_str().unwrap_or("") {
            "thread.started" if !s("thread_id").is_empty() => out.push(CliEvent::Session(s("thread_id"))),
            "item.completed" => {
                let item = &v["item"];
                let text = item["text"].as_str().unwrap_or("").to_string();
                match item["type"].as_str().unwrap_or("") {
                    "agent_message" if !text.is_empty() => out.push(CliEvent::Text(format!("{text}\n"))),
                    "reasoning" if !text.is_empty() => out.push(CliEvent::Thinking(format!("{text}\n"))),
                    "error" if !text.is_empty() || item["message"].is_string() => {
                        out.push(CliEvent::Notice(format!("Codex: {}", item["message"].as_str().unwrap_or(&text))));
                    }
                    _ => {}
                }
            }
            "turn.completed" => out.push(CliEvent::Finished(None)),
            "turn.failed" => {
                let why = v["error"]["message"].as_str().unwrap_or("the request failed").to_string();
                out.push(CliEvent::Finished(Some(format!("Codex: {why}"))));
            }
            // passing trouble Codex retries by itself ("Reconnecting... 2/5")
            "error" => {
                let m = s("message");
                if !m.is_empty() && !m.starts_with("Reconnecting") {
                    out.push(CliEvent::Notice(format!("Codex: {m}")));
                }
            }
            _ => {}
        },
        // init, step_update (user_input, agent_response with text_delta, tool, checkpoint), result
        Cli::Antigravity => match v["event"].as_str().unwrap_or("") {
            "init" if !s("conversation_id").is_empty() => out.push(CliEvent::Session(s("conversation_id"))),
            "step_update" => {
                let st = &v["step_update"];
                let delta = st["text_delta"].as_str().unwrap_or("");
                match st["step_type"].as_str().unwrap_or("") {
                    "agent_response" if !delta.is_empty() => out.push(CliEvent::Text(delta.to_string())),
                    "thinking" | "planner_response" if !delta.is_empty() => out.push(CliEvent::Thinking(delta.to_string())),
                    // its own tools refused by the settings: say so, Claude-style
                    "tool" if st["state"] == "DONE" && st["tool_info"]["error"]["message"].is_string() && st["tool_info"]["name"] != "call_mcp_tool" => {
                        out.push(CliEvent::Notice(format!("Antigravity: {} refused ({})", st["tool_info"]["name"].as_str().unwrap_or("a tool"), st["tool_info"]["error"]["message"].as_str().unwrap_or(""))));
                    }
                    _ => {}
                }
            }
            "result" => {
                let r = &v["result"];
                let ok = r["status"].as_str().is_some_and(|st| st == "SUCCESS");
                let why = r["error"]["message"].as_str().or(r["error"].as_str()).or(r["status"].as_str()).unwrap_or("unknown error");
                out.push(CliEvent::Finished((!ok).then(|| format!("Antigravity: {why}"))));
            }
            _ => {}
        },
    }
    out
}

impl Chat {
    /// One request through the Gemini CLI or Codex. Follow-ups resume the same conversation.
    pub(super) fn cli_turn(&mut self, cli: Cli, prompt: &str, images: &[Vec<u8>], settings: &Settings, stop: &AtomicBool, send: &dyn Fn(FromWorker)) -> Result<(), String> {
        let bridge = match self.bridge.take() {
            Some(b) => b,
            None => Bridge::start()?,
        };
        bridge.refuse_pending();
        let r = self.cli_child(cli, &bridge, prompt, images, settings, stop, send);
        bridge.refuse_pending();
        self.bridge = Some(bridge);
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn cli_child(&mut self, cli: Cli, bridge: &Bridge, prompt: &str, images: &[Vec<u8>], settings: &Settings, stop: &AtomicBool, send: &dyn Fn(FromWorker)) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let private = settings_path().and_then(|p| p.parent().map(|d| d.to_path_buf())).filter(|d| std::fs::create_dir_all(d).is_ok()).ok_or("no config folder (HOME is not set)")?;
        let folder = match cli {
            Cli::Gemini => gemini_folder(&exe)?,
            Cli::Antigravity => agy_folder(&exe)?,
            Cli::Codex => {
                codex_profile(&exe)?;
                let d = private.join("codex-run");
                std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
                d
            }
        };
        let session = self.cc_session.as_deref().and_then(|id| id.strip_prefix(cli.prefix())).map(String::from);
        let mut prompt = prompt.to_string();
        // pictures go as files Codex attaches; the Gemini CLI has no way to take one headless
        let mut picture: Vec<std::path::PathBuf> = vec![];
        match cli {
            Cli::Codex => {
                for (k, b) in images.iter().enumerate() {
                    let ext = if crate::claude::image_type(b) == "image/jpeg" { "jpg" } else { "png" };
                    let p = folder.join(format!("picture-{}-{k}.{ext}", std::process::id()));
                    write_private_bytes(&p, b).map_err(|e| format!("could not save a picture for Codex: {e}"))?;
                    picture.push(p);
                }
            }
            Cli::Gemini | Cli::Antigravity if !images.is_empty() => {
                send(FromWorker::Notice(format!("The {} can't be sent pictures, so it gets this request as text only.", cli.label())));
                prompt.push_str("\n\n(The user sent pictures with this request, a pencil drawing on the part or photos; they couldn't be passed on. Faces under pencil strokes are listed in the editor block.)");
            }
            _ => {}
        }
        let mut cmd = command(cli);
        cmd.args(args(cli, &folder, settings, session.as_deref(), &picture));
        cmd.env(crate::agent::BRIDGE_TOKEN_ENV, &bridge.token).env(crate::agent::BRIDGE_ADDR_ENV, &bridge.addr);
        if cli == Cli::Gemini {
            // the run folder is rCAD's own, so its settings (the cad server) apply
            cmd.env("GEMINI_CLI_TRUST_WORKSPACE", "true");
        }
        cmd.current_dir(&folder).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { cli.install_hint() } else { format!("could not start the {}: {e}", cli.label()) })?;
        if let Some(mut input) = child.stdin.take() {
            let prompt = stdin_text(cli, &prompt);
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = input.write_all(prompt.as_bytes());
                // closing stdin ends the request
            });
        }
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
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Stopped".to_string());
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
                    for ev in parse(cli, &v) {
                        match ev {
                            CliEvent::Session(id) => self.cc_session = Some(format!("{}{id}", cli.prefix())),
                            CliEvent::Text(t) => send(FromWorker::Text(t)),
                            CliEvent::Thinking(t) => send(FromWorker::Thinking(t)),
                            CliEvent::Notice(t) => send(FromWorker::Notice(t)),
                            CliEvent::Finished(e) => {
                                finished = true;
                                if e.is_some() {
                                    error = e;
                                }
                            }
                        }
                    }
                }
                Ok(CcLine::End) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        for p in &picture {
            let _ = std::fs::remove_file(p);
        }
        result?;
        let status = child.wait().map_err(|e| e.to_string())?;
        let err_text = err_text.join().unwrap_or_default();
        if let Some(e) = error {
            return Err(e);
        }
        if !finished || !status.success() {
            let tail: String = err_text
                .trim()
                .lines()
                .filter(|l| !l.trim_start().starts_with("at ") && !l.contains("256-color") && !l.contains("YOLO mode"))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            if session.is_some() {
                // a resumed conversation that is gone: start a fresh one next time
                self.cc_session = None;
            }
            return Err(format!("{} stopped ({status}){}", cli.label(), if tail.is_empty() { String::new() } else { format!(": {tail}") }));
        }
        Ok(())
    }
}

fn write_private_bytes(p: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
        f.write_all(data)
    }
    #[cfg(not(unix))]
    std::fs::write(p, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(cli: Cli, text: &str) -> Vec<CliEvent> {
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).flat_map(|v| parse(cli, &v)).collect()
    }

    #[test]
    fn gemini_stream_json_gives_the_session_text_and_end() {
        // as printed by gemini 0.70 -o stream-json
        let out = r#"{"type":"init","timestamp":"t","session_id":"fa67909e-63c3-400e-869a-d039ae9fa62a","model":"auto"}
{"type":"message","timestamp":"t","role":"user","content":"make a plate"}
{"type":"message","timestamp":"t","role":"assistant","content":"Looking at ","delta":true}
{"type":"tool_use","timestamp":"t","tool_name":"mcp_cad_describe","tool_id":"x","parameters":{}}
{"type":"tool_result","timestamp":"t","tool_id":"x","status":"success","output":"ok"}
{"type":"message","timestamp":"t","role":"assistant","content":"the plate.","delta":true}
{"type":"result","timestamp":"t","status":"success","stats":{"total_tokens":7}}"#;
        assert_eq!(
            lines(Cli::Gemini, out),
            vec![
                CliEvent::Session("fa67909e-63c3-400e-869a-d039ae9fa62a".into()),
                CliEvent::Text("Looking at ".into()),
                CliEvent::Text("the plate.".into()),
                CliEvent::Finished(None)
            ]
        );
        let failed = r#"{"type":"result","status":"error","error":{"type":"FatalCancellationError","message":"Operation cancelled."}}"#;
        assert_eq!(lines(Cli::Gemini, failed), vec![CliEvent::Finished(Some("Gemini CLI: Operation cancelled.".into()))]);
    }

    #[test]
    fn codex_json_gives_the_thread_text_and_end() {
        // as printed by codex-cli 0.171 exec --json
        let out = r#"{"type":"thread.started","thread_id":"01a20a58-f6d2-72a2-929b-46c5b76e8007"}
{"type":"turn.started"}
{"type":"error","message":"Reconnecting... 2/5 (stream disconnected before completion)"}
{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"Checking the sketch"}}
{"type":"item.started","item":{"id":"item_1","type":"mcp_tool_call","server":"cad","tool":"describe","arguments":{},"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"mcp_tool_call","server":"cad","tool":"describe","arguments":{},"status":"completed"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Made a plate."}}
{"type":"turn.completed","usage":{"input_tokens":201,"output_tokens":21}}"#;
        assert_eq!(
            lines(Cli::Codex, out),
            vec![
                CliEvent::Session("01a20a58-f6d2-72a2-929b-46c5b76e8007".into()),
                CliEvent::Thinking("Checking the sketch\n".into()),
                CliEvent::Text("Made a plate.\n".into()),
                CliEvent::Finished(None)
            ]
        );
        let failed = r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#;
        assert_eq!(lines(Cli::Codex, failed), vec![CliEvent::Finished(Some("Codex: You've hit your usage limit.".into()))]);
    }

    #[test]
    fn codex_resumes_with_the_id_after_the_options_and_reads_stdin() {
        let s = Settings { auth: Auth::Codex, codex_model: "gpt-5.5".into(), effort: "max".into(), ..Settings::default() };
        let a = args(Cli::Codex, std::path::Path::new("/r"), &s, Some("abc-1"), &[std::path::PathBuf::from("/r/d.png")]);
        assert_eq!(a, ["--profile", "rcad", "exec", "resume", "--json", "--skip-git-repo-check", "--model", "gpt-5.5", "-c", "model_reasoning_effort=xhigh", "--image=/r/d.png", "--", "abc-1", "-"]);
        let a = args(Cli::Codex, std::path::Path::new("/r"), &Settings { auth: Auth::Codex, ..Settings::default() }, None, &[]);
        assert_eq!(a, ["--profile", "rcad", "exec", "--json", "--skip-git-repo-check", "-c", "model_reasoning_effort=high", "--", "-"]);
    }

    #[test]
    fn gemini_runs_headless_with_only_the_cad_server() {
        let s = Settings { auth: Auth::Gemini, ..Settings::default() };
        let a = args(Cli::Gemini, std::path::Path::new("/r"), &s, Some("fa67"), &[]);
        assert_eq!(a[..2], ["-p", ""]);
        assert!(a.windows(2).any(|w| w == ["--allowed-mcp-server-names", "cad"]));
        assert!(a.windows(2).any(|w| w == ["--resume", "fa67"]));
        assert!(!a.iter().any(|x| x == "--model"), "no model: the CLI's own choice");
    }

    #[test]
    fn antigravity_stream_json_gives_the_conversation_text_and_end() {
        let out = r#"{"event":"init","conversation_id":"c195","init":{"cwd":"/r","tools":["call_mcp_tool"],"permission_mode":"request-review"}}
{"event":"step_update","step_update":{"conversation_id":"c195","step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"conversation_id":"c195","step_index":2,"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"name":"call_mcp_tool","parameters":{"ServerName":"cad","ToolName":"describe"},"output":"{}"}}}
{"event":"step_update","step_update":{"conversation_id":"c195","step_index":3,"state":"ACTIVE","step_type":"agent_response","text_delta":"A plate "}}
{"event":"step_update","step_update":{"conversation_id":"c195","step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"with a hole.\n"}}
{"event":"step_update","step_update":{"conversation_id":"c195","step_index":4,"state":"DONE","step_type":"tool","tool_info":{"name":"run_command","error":{"type":"denied","message":"Matches user-configured deny rule"}}}}
{"event":"result","result":{"conversation_id":"c195","status":"SUCCESS","response":"A plate with a hole.\n","num_turns":1}}"#;
        assert_eq!(
            lines(Cli::Antigravity, out),
            vec![
                CliEvent::Session("c195".into()),
                CliEvent::Text("A plate ".into()),
                CliEvent::Text("with a hole.\n".into()),
                CliEvent::Notice("Antigravity: run_command refused (Matches user-configured deny rule)".into()),
                CliEvent::Finished(None),
            ]
        );
        let failed = r#"{"event":"result","result":{"conversation_id":"c195","status":"ERROR","error":{"message":"authentication required"}}}"#;
        assert_eq!(lines(Cli::Antigravity, failed), vec![CliEvent::Finished(Some("Antigravity: authentication required".into()))]);
    }

    #[test]
    fn antigravity_reads_the_request_from_stdin_with_only_the_cad_tools() {
        let s = Settings { auth: Auth::Antigravity, antigravity_model: "gemini-3.8-flash-medium".into(), effort: "high".into(), ..Settings::default() };
        let a = args(Cli::Antigravity, std::path::Path::new("/r"), &s, Some("c195"), &[]);
        assert_eq!(a[..4], ["--input-format", "stream-json", "--output-format", "stream-json"]);
        let set: Value = serde_json::from_str(&a[5]).unwrap();
        assert_eq!(set["agentMode"], "");
        assert_eq!(set["permissions"]["allow"], json!(["mcp(cad/*)"]));
        assert!(set["permissions"]["deny"].as_array().unwrap().contains(&json!("command(*)")));
        assert_eq!(a[6..], ["--model", "gemini-3.8-flash-medium", "--conversation", "c195", "-p="]);
        let a = args(Cli::Antigravity, std::path::Path::new("/r"), &Settings { auth: Auth::Antigravity, effort: "high".into(), ..Settings::default() }, None, &[]);
        assert_eq!(a[6..], ["--effort", "high", "-p="]);
        let line: Value = serde_json::from_str(stdin_text(Cli::Antigravity, "make a \"box\"").trim()).unwrap();
        assert_eq!(line, json!({"event": "user", "message": {"content": "make a \"box\""}}));
        assert_eq!(session_cli("agy:c195"), Some(Cli::Antigravity));
    }

    #[test]
    fn saved_ids_say_which_program_they_belong_to() {
        assert_eq!(session_cli("gemini:fa67"), Some(Cli::Gemini));
        assert_eq!(session_cli("codex:01a2"), Some(Cli::Codex));
        assert_eq!(session_cli("3f2c-claude-code-id"), None);
    }

    #[test]
    fn toml_strings_escape_quotes_and_newlines() {
        assert_eq!(toml_str("a \"b\"\nc\\d"), r#""a \"b\"\nc\\d""#);
    }
}
