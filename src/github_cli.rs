//! `rcad github …`: the GitHub account and projects with no window, for the rCAD phone app (the
//! netdash service runs these). The sign-in is the editor's own (Settings > GitHub): the same
//! saved token, the GitHub CLI's or `GITHUB_TOKEN` when there is none, so signing in on the phone
//! signs the editor in too.
//!
//! Each command prints one JSON object on stdout; a failure is `{"error": "…"}` and exit code 1.
//!
//! ```text
//! rcad github status                       {"signed_in", "login", "name", "source": "saved"|"env"|"gh"}
//! rcad github login-start                  {"user_code", "verification_uri", "device_code", "interval", "expires_in"}
//! rcad github login-poll --code D          {"state": "waiting"|"slow_down"|"done", "login"}
//! rcad github logout                       {"ok": true, "signed_in": …}  (the GitHub CLI's token may still apply)
//! rcad github repos                        {"repos": [{"full_name", "name", "private", "description", "clone_url", "pushed", "rcad"}]}
//! rcad github clone --url U --into DIR     {"doc": path, "empty": bool}  (empty: no document yet, make one at doc)
//! rcad github sync --doc P [--message M]   {"committed", "synced", "diverged"}  commit (when it commits on save), then sync
//! rcad github resolve --doc P --keep mine|theirs   after a sync that couldn't combine both sides
//! ```

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::vcs::{self, Synced, TokenSource};

const USAGE: &str = "usage: rcad github status | login-start | login-poll --code D | logout | repos | clone --url U --into DIR | sync --doc P [--message M] | resolve --doc P --keep mine|theirs";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn token() -> Result<String, String> {
    vcs::token().map(|(t, _)| t).ok_or_else(|| "sign in to GitHub first".to_string())
}

fn status() -> Result<Value, String> {
    let Some((t, src)) = vcs::token() else {
        return Ok(json!({"signed_in": false}));
    };
    let source = match src {
        TokenSource::Saved => "saved",
        TokenSource::Env => "env",
        TokenSource::GhCli => "gh",
    };
    match vcs::gh_user(&t) {
        Ok(u) => Ok(json!({"signed_in": true, "login": u.login, "name": u.name, "source": source})),
        Err(e) => Ok(json!({"signed_in": false, "source": source, "error": e})),
    }
}

fn who() -> vcs::Identity {
    let u = vcs::token().and_then(|(t, _)| vcs::gh_user(&t).ok());
    vcs::Identity::guess(u.as_ref())
}

fn synced(r: Synced) -> Value {
    match r {
        Synced::UpToDate => json!({"synced": "Up to date with GitHub"}),
        Synced::Pulled(s) | Synced::Pushed(s) | Synced::Merged(s) => json!({"synced": s}),
        Synced::Diverged(d) => {
            let list = |c: &[vcs::Commit]| c.iter().map(|c| json!({"subject": c.subject, "author": c.author, "time": c.time})).collect::<Vec<_>>();
            json!({"diverged": {"mine": list(&d.mine), "theirs": list(&d.theirs)}})
        }
    }
}

/// After the document `doc` was saved outside the editor (a phone chat): commit it when its
/// repository commits on save, then sync when it syncs, as the editor does after a save. The
/// result has `committed` (the commit's subject), `synced` (what the sync did), `diverged` (both
/// sides changed: the user picks one) and `error`; an empty object when the document isn't in a
/// repository that does either.
pub fn commit_and_sync(doc: &Path, note: Option<&str>) -> Value {
    let mut out = json!({});
    let Some(s) = vcs::status(doc) else { return out };
    let who = who();
    if s.changed && s.autocommit {
        let mut msg = std::fs::read_to_string(doc)
            .ok()
            .and_then(|t| crate::workspace::Workspace::from_json(&t).ok())
            .map(|ws| vcs::save_message(&s.root, &s.rel, &ws))
            .unwrap_or_else(|| "Save".into());
        if let Some(n) = note.map(str::trim).filter(|n| !n.is_empty()) {
            msg.push_str(&format!("\n\n{n}"));
        }
        match vcs::commit(&s.root, &s.rel, &msg, false, &who) {
            Ok(Some(c)) => out["committed"] = json!(c.subject),
            Ok(None) => {}
            Err(e) => {
                out["error"] = json!(format!("could not commit: {e}"));
                return out;
            }
        }
    }
    if s.sync {
        let t = vcs::token().map(|(t, _)| t);
        match vcs::sync(&s.root, t.as_deref(), &who) {
            Ok(r) => {
                if let Value::Object(m) = synced(r) {
                    out.as_object_mut().unwrap().extend(m);
                }
            }
            Err(e) => out["error"] = json!(format!("could not sync with GitHub: {e}")),
        }
    }
    out
}

/// Bring in what was pushed from elsewhere before working on `doc` (nothing when it doesn't sync,
/// or has changes that aren't committed). Returns what it did, for a notice.
pub fn pull_first(doc: &Path) -> Option<String> {
    let s = vcs::status(doc)?;
    if !s.sync || s.changed {
        return None;
    }
    let t = vcs::token().map(|(t, _)| t);
    match vcs::sync(&s.root, t.as_deref(), &who()) {
        Ok(Synced::Pulled(m)) | Ok(Synced::Merged(m)) => Some(m),
        Ok(Synced::Diverged(_)) => Some("This copy and GitHub both changed the project: sync it from the phone's menu to pick one".into()),
        Ok(_) => None,
        Err(e) => Some(format!("Could not sync with GitHub first: {e}")),
    }
}

fn run(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    match sub {
        "status" => status(),
        "login-start" => {
            let id = vcs::client_id().ok_or("this rcad was built without a GitHub OAuth app")?;
            let c = vcs::device_start(&id)?;
            Ok(json!({"user_code": c.user_code, "verification_uri": c.verification_uri, "device_code": c.device_code, "interval": c.interval, "expires_in": c.expires_in}))
        }
        "login-poll" => {
            let code = flag(rest, "--code").ok_or("give --code")?;
            let id = vcs::client_id().ok_or("this rcad was built without a GitHub OAuth app")?;
            match vcs::device_poll(&id, &code)? {
                vcs::DevicePoll::Waiting => Ok(json!({"state": "waiting"})),
                vcs::DevicePoll::SlowDown => Ok(json!({"state": "slow_down"})),
                vcs::DevicePoll::Token(t) => {
                    let u = vcs::gh_user(&t)?;
                    vcs::save_token(&t)?;
                    Ok(json!({"state": "done", "login": u.login, "name": u.name}))
                }
            }
        }
        "logout" => {
            vcs::save_token("")?;
            Ok(json!({"ok": true, "signed_in": vcs::token().is_some()}))
        }
        "repos" => {
            let list = vcs::gh_repos(&token()?)?;
            let repos: Vec<Value> = list
                .into_iter()
                .map(|r| json!({"full_name": r.full_name, "name": r.name, "private": r.private, "description": r.description, "clone_url": r.clone_url, "pushed": r.pushed, "rcad": r.rcad}))
                .collect();
            Ok(json!({"repos": repos}))
        }
        "clone" => {
            let url = flag(rest, "--url").ok_or("give --url")?;
            let into = PathBuf::from(flag(rest, "--into").ok_or("give --into")?);
            let t = vcs::token().map(|(t, _)| t);
            match vcs::clone_project(&url, t.as_deref(), &into)? {
                vcs::Cloned::Document(p) => {
                    // a copy that was already here: bring it up to date
                    let mut v = json!({"doc": p, "empty": false});
                    if let Some(m) = pull_first(&p) {
                        v["synced"] = json!(m);
                    }
                    Ok(v)
                }
                vcs::Cloned::Empty(p) => Ok(json!({"doc": p, "empty": true})),
            }
        }
        "sync" => {
            let doc = PathBuf::from(flag(rest, "--doc").ok_or("give --doc")?);
            if vcs::status(&doc).is_none() {
                return Err(format!("{} isn't in a git repository", doc.display()));
            }
            let v = commit_and_sync(&doc, flag(rest, "--message").as_deref());
            match v["error"].as_str() {
                Some(e) => Err(e.to_string()),
                None => Ok(v),
            }
        }
        "resolve" => {
            let doc = PathBuf::from(flag(rest, "--doc").ok_or("give --doc")?);
            let mine = match flag(rest, "--keep").as_deref() {
                Some("mine") => true,
                Some("theirs") => false,
                _ => return Err("give --keep mine or --keep theirs".into()),
            };
            let s = vcs::status(&doc).ok_or_else(|| format!("{} isn't in a git repository", doc.display()))?;
            let t = vcs::token().map(|(t, _)| t);
            Ok(json!({"synced": vcs::resolve(&s.root, t.as_deref(), &who(), mine)?}))
        }
        "" | "help" | "--help" | "-h" => Err(USAGE.into()),
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            println!("{}", json!({"error": e}));
            1
        }
    }
}
