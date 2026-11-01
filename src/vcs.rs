//! Version control for documents with git. A document's folder becomes a repository, saves are
//! committed (each with a message that says what changed), earlier commits can be compared or
//! opened again, and the repository can be published to GitHub and pushed or pulled.
//!
//! Everything runs the `git` program, so the user's own git settings and sign-ins (SSH keys,
//! credential managers) apply. GitHub calls use a personal access token, kept in
//! `~/.config/cad-rust-app/github.json` or taken from `GITHUB_TOKEN`, `GH_TOKEN` or `gh auth token`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use crate::workspace::Workspace;

/// One commit in a document's history.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    /// Unix seconds.
    pub time: u64,
    pub subject: String,
}

/// Where a document stands in its repository.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The repository's top folder.
    pub root: PathBuf,
    /// The document's path inside the repository, with forward slashes.
    pub rel: String,
    pub branch: String,
    pub has_commits: bool,
    /// The document on disk differs from the last commit (or was never committed).
    pub changed: bool,
    /// Other changed or new files in the repository.
    pub others: Vec<String>,
    pub remote: Option<String>,
    /// Commits not yet pushed and not yet pulled, when the branch has an upstream.
    pub upstream: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Commit on every save.
    pub autocommit: bool,
}

// ------------------------------------------------------------ running git

fn command(dir: &Path, args: &[&str], env: &[(String, String)]) -> Command {
    let mut c = Command::new("git");
    // never take a folder that merely looks like a bare repository (one unpacked from a ZIP, say)
    // as the repository: its config could run commands
    c.current_dir(dir).args(["-c", "safe.bareRepository=explicit"]).args(args).stdin(Stdio::null()).env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C");
    for (k, v) in env {
        c.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    c
}

/// Run git in `dir` and return its output, or its error message.
fn git_env(dir: &Path, args: &[&str], env: &[(String, String)]) -> Result<String, String> {
    let out = command(dir, args, env)
        .output()
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "git is not installed".to_string() } else { format!("could not run git: {e}") })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg: Vec<&str> = err.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("hint:")).collect();
        let msg = msg.join(" ").replace("fatal: ", "").replace("error: ", "");
        Err(if msg.is_empty() { format!("git {} failed", args.first().unwrap_or(&"")) } else { msg })
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_env(dir, args, &[])
}

/// Whether git exits with status 0 (for the `--quiet` checks).
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    command(dir, args, &[]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// The installed git's version, e.g. "2.43.0", or None without git.
pub fn version() -> Option<String> {
    let out = command(&std::env::temp_dir(), &["--version"], &[]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().nth(2).map(String::from)
}

fn folder_of(file: &Path) -> PathBuf {
    match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The repository holding `file` and the file's path inside it, if it is in one.
pub fn locate(file: &Path) -> Option<(PathBuf, String)> {
    let top = git(&folder_of(file), &["rev-parse", "--show-toplevel"]).ok()?;
    let root = std::fs::canonicalize(top.trim()).ok()?;
    let full = std::fs::canonicalize(file).ok()?;
    let rel = full.strip_prefix(&root).ok()?;
    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
    Some((root, rel))
}

static TRACKED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>> = std::sync::OnceLock::new();

/// Whether a document is in a git repository, from a cache so the start screen can ask every frame.
pub fn is_tracked_cached(file: &Path) -> bool {
    let cache = TRACKED.get_or_init(Default::default);
    if let Some(&t) = cache.lock().unwrap().get(file) {
        return t;
    }
    // a .git folder (or file, for worktrees) here or above, without running git
    let tracked = file.ancestors().skip(1).any(|d| d.join(".git").exists());
    cache.lock().unwrap().insert(file.to_path_buf(), tracked);
    tracked
}

/// Start tracking a document: make its folder a repository (unless it is in one already, and
/// it isn't a project folder inside another repository), turn on commit-on-save and commit the document.
pub fn init(file: &Path, who: &Identity) -> Result<Status, String> {
    let dir = folder_of(file);
    // a project folder (named after its document) gets a repository of its own, even inside
    // another one (a home folder kept in git, say), so its commits and pushes stay its own
    let own = match locate(file) {
        None => true,
        Some((root, _)) => crate::project::is_project_folder(file) && std::fs::canonicalize(&dir).ok() != Some(root),
    };
    if own {
        git(&dir, &["init", "-q"])?;
        // "main" whatever init.defaultBranch says (git init -b needs git 2.28)
        git(&dir, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "# temporary files\n*.tmp\n.DS_Store\nThumbs.db\n");
        }
    }
    let (root, rel) = locate(file).ok_or("the folder could not be made a git repository")?;
    if let Some(c) = TRACKED.get() {
        c.lock().unwrap().clear();
    }
    git(&root, &["config", "rcad.autocommit", "true"])?;
    let name = file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let all = !git_ok(&root, &["rev-parse", "-q", "--verify", "HEAD"]) && root.join(".gitignore").exists();
    commit(&root, &rel, &format!("Start tracking {name}"), false, who)?;
    if all {
        let _ = commit(&root, ".gitignore", "Ignore temporary files", false, who);
    }
    status(file).ok_or_else(|| "the repository could not be read".into())
}

/// Settings kept from a repository's config when a project is imported: the ones that make it
/// work and find its remote. The rest (core.fsmonitor, core.hooksPath, filters, core.sshCommand
/// and the like) can run commands, so an archive from someone else doesn't get to set them.
fn keep_imported(key: &str) -> bool {
    matches!(
        key,
        "core.repositoryformatversion" | "core.filemode" | "core.ignorecase" | "core.symlinks" | "core.precomposeunicode" | "core.logallrefupdates" | "extensions.objectformat" | "rcad.autocommit" | "user.name" | "user.email" | "remote.origin.url" | "remote.origin.fetch"
    ) || (key.starts_with("branch.") && (key.ends_with(".remote") || key.ends_with(".merge")))
}

/// Write `git_dir`'s config from an imported repository's `config`, keeping only the safe settings.
pub fn write_imported_config(git_dir: &Path, config: Option<&[u8]>) -> Result<(), String> {
    let out = git_dir.join("config");
    let tmp = git_dir.join("config.imported");
    // read with `git config -f` (which only reads the file), from outside the project
    let mut entries = vec![];
    if let Some(c) = config {
        std::fs::write(&tmp, c).map_err(|e| e.to_string())?;
        let list = git(&std::env::temp_dir(), &["config", "-f", &tmp.to_string_lossy(), "-z", "--get-regexp", "."]);
        let _ = std::fs::remove_file(&tmp);
        for e in list.unwrap_or_default().split('\0') {
            if let Some((k, v)) = e.split_once('\n') {
                if keep_imported(&k.to_ascii_lowercase()) {
                    entries.push((k.to_string(), v.to_string()));
                }
            }
        }
    }
    // written by hand, so the repository works even without git to write the rest
    let version = entries.iter().find(|(k, _)| k.eq_ignore_ascii_case("core.repositoryformatversion")).map(|(_, v)| v.trim()).filter(|v| *v == "0" || *v == "1").unwrap_or("0");
    std::fs::write(&out, format!("[core]\n\trepositoryformatversion = {version}\n")).map_err(|e| e.to_string())?;
    let file = out.to_string_lossy().into_owned();
    for (k, v) in entries.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("core.repositoryformatversion")) {
        // (best effort: a setting git won't write back is left out)
        let _ = git(&std::env::temp_dir(), &["config", "-f", &file, "--add", k, v]);
    }
    Ok(())
}

/// How many other things the document's folder holds (counting to 100), to suggest a folder of
/// its own before making a busy folder (Documents, say) a repository.
pub fn folder_neighbours(file: &Path) -> usize {
    std::fs::read_dir(folder_of(file)).map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path() != file).take(100).count()).unwrap_or(0)
}

/// Move the document into a new folder named after it, next to where it is, and return its new
/// path (to track that folder rather than a busy one).
pub fn move_into_own_folder(file: &Path) -> Result<PathBuf, String> {
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let mut dir = folder_of(file).join(&stem);
    let mut n = 2;
    while dir.exists() {
        dir = folder_of(file).join(format!("{stem} {n}"));
        n += 1;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let to = dir.join(file.file_name().unwrap_or_default());
    std::fs::rename(file, &to).map_err(|e| format!("could not move the document: {e}"))?;
    Ok(to)
}

/// Who commits are by, for repositories where git has no user set.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    /// The signed-in GitHub account's name and no-reply address, else the system user name.
    pub fn guess(gh: Option<&GhUser>) -> Identity {
        match gh {
            Some(u) => Identity { name: u.name.clone().unwrap_or_else(|| u.login.clone()), email: format!("{}+{}@users.noreply.github.com", u.id, u.login) },
            None => {
                let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "rcad".into());
                Identity { name: user.clone(), email: format!("{user}@localhost") }
            }
        }
    }
}

/// Set the repository's commit author when git has none configured, so commits don't fail.
fn ensure_identity(root: &Path, who: &Identity) -> Result<(), String> {
    if git(root, &["config", "user.name"]).is_err() {
        git(root, &["config", "user.name", &who.name])?;
    }
    if git(root, &["config", "user.email"]).is_err() {
        git(root, &["config", "user.email", &who.email])?;
    }
    Ok(())
}

/// Commit the document (`all`: every changed file in the repository). Returns the new commit,
/// or None when there was nothing to commit.
pub fn commit(root: &Path, rel: &str, message: &str, all: bool, who: &Identity) -> Result<Option<Commit>, String> {
    ensure_identity(root, who)?;
    if all {
        git(root, &["add", "-A"])?;
    } else {
        git(root, &["add", "--", rel])?;
    }
    let unchanged = if all { git_ok(root, &["diff", "--cached", "--quiet"]) } else { git_ok(root, &["diff", "--cached", "--quiet", "--", rel]) };
    if unchanged && git_ok(root, &["rev-parse", "-q", "--verify", "HEAD"]) {
        return Ok(None);
    }
    let message = if message.trim().is_empty() { "Save" } else { message.trim() };
    if all {
        git(root, &["commit", "-q", "-m", message])?;
    } else {
        git(root, &["commit", "-q", "-m", message, "--", rel])?;
    }
    Ok(history(root, "", 1).into_iter().next())
}

const SEP: char = '\u{1f}';
const END: char = '\u{1e}';

/// The commits that touched the document, newest first (`rel` empty: the whole repository).
pub fn history(root: &Path, rel: &str, max: usize) -> Vec<Commit> {
    let n = format!("-n{max}");
    let fmt = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e";
    let out = if rel.is_empty() { git(root, &["log", &n, fmt]) } else { git(root, &["log", &n, "--follow", fmt, "--", rel]) };
    out.unwrap_or_default()
        .split(END)
        .filter_map(|e| {
            let f: Vec<&str> = e.trim_start_matches('\n').split(SEP).collect();
            (f.len() == 5).then(|| Commit { hash: f[0].into(), short: f[1].into(), author: f[2].into(), time: f[3].parse().unwrap_or(0), subject: f[4].into() })
        })
        .collect()
}

/// The document as it was in a commit.
pub fn file_at(root: &Path, hash: &str, rel: &str) -> Result<String, String> {
    // the path may have been different then (history follows renames)
    // -z: the name as it is (without it git quotes names with accents and the like)
    let names = git(root, &["log", "-n1", "--follow", "-z", "--name-only", "--format=", hash, "--", rel]).unwrap_or_default();
    let path = names.split(['\0', '\n']).find(|l| !l.is_empty()).unwrap_or(rel).to_string();
    git(root, &["show", &format!("{hash}:{path}")])
}

/// Where the document stands: branch, changes, remote and how far ahead or behind it is.
pub fn status(file: &Path) -> Option<Status> {
    let (root, rel) = locate(file)?;
    let mut s = Status { root: root.clone(), rel: rel.clone(), ..Default::default() };
    s.branch = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]).map(|b| b.trim().to_string()).unwrap_or_else(|_| "detached".into());
    s.has_commits = git_ok(&root, &["rev-parse", "-q", "--verify", "HEAD"]);
    // NUL separated: "XY path", with renames followed by their old path
    let porcelain = git(&root, &["status", "--porcelain", "-z", "--untracked-files=all"]).unwrap_or_default();
    let mut entries = porcelain.split('\0');
    while let Some(e) = entries.next() {
        if e.len() < 4 {
            continue;
        }
        let path = &e[3..];
        if matches!(e.as_bytes()[0], b'R' | b'C') {
            entries.next();
        }
        if path == rel {
            s.changed = true;
        } else {
            s.others.push(path.to_string());
        }
    }
    s.remote = git(&root, &["remote", "get-url", "origin"]).ok().map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    if let Ok(counts) = git(&root, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"]) {
        let mut it = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
        s.upstream = true;
        s.behind = it.next().unwrap_or(0);
        s.ahead = it.next().unwrap_or(0);
    }
    s.autocommit = git(&root, &["config", "--bool", "rcad.autocommit"]).map(|v| v.trim() == "true").unwrap_or(false);
    Some(s)
}

pub fn set_autocommit(root: &Path, on: bool) -> Result<(), String> {
    git(root, &["config", "rcad.autocommit", if on { "true" } else { "false" }]).map(|_| ())
}

/// Point `origin` at `url`.
pub fn set_remote(root: &Path, url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("enter the repository's URL".into());
    }
    if git(root, &["remote", "get-url", "origin"]).is_ok() { git(root, &["remote", "set-url", "origin", url]) } else { git(root, &["remote", "add", "origin", url]) }.map(|_| ())
}

/// Environment that signs git's HTTPS requests to github.com in with the token. It goes through
/// GIT_CONFIG_* (git 2.31+) so the token stays out of the command line and the repository's config.
fn auth_env(token: Option<&str>, remote: &str) -> Vec<(String, String)> {
    use base64::Engine;
    let Some(token) = token.filter(|t| !t.is_empty()) else { return vec![] };
    if !remote.starts_with("https://github.com/") {
        return vec![];
    }
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    vec![
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "http.https://github.com/.extraheader".into()),
        ("GIT_CONFIG_VALUE_0".into(), format!("AUTHORIZATION: basic {basic}")),
    ]
}

/// Push the branch to `origin` (setting it as the upstream).
pub fn push(root: &Path, token: Option<&str>) -> Result<String, String> {
    let remote = git(root, &["remote", "get-url", "origin"]).map_err(|_| "the project has no remote yet: publish it to GitHub first".to_string())?;
    let branch = git(root, &["symbolic-ref", "--short", "-q", "HEAD"]).map_err(|_| "not on a branch".to_string())?;
    let branch = branch.trim();
    git_env(root, &["push", "-u", "origin", branch], &auth_env(token, remote.trim())).map_err(|e| friendly(&e))?;
    Ok(format!("Pushed {branch} to {}", short_remote(remote.trim())))
}

/// Bring in the remote's commits when that is a fast-forward (never a merge).
pub fn pull(root: &Path, token: Option<&str>) -> Result<String, String> {
    let remote = git(root, &["remote", "get-url", "origin"]).map_err(|_| "the project has no remote".to_string())?;
    let branch = git(root, &["symbolic-ref", "--short", "-q", "HEAD"]).map_err(|_| "not on a branch".to_string())?;
    let branch = branch.trim();
    let before = git(root, &["rev-parse", "-q", "--verify", "HEAD"]).unwrap_or_default();
    git_env(root, &["pull", "--ff-only", "origin", branch], &auth_env(token, remote.trim())).map_err(|e| friendly(&e))?;
    let after = git(root, &["rev-parse", "-q", "--verify", "HEAD"]).unwrap_or_default();
    Ok(if before == after { "Already up to date".into() } else { format!("Pulled the latest {branch} from {}", short_remote(remote.trim())) })
}

/// git's error messages for the usual push and pull failures, said plainly.
fn friendly(e: &str) -> String {
    if e.contains("Not possible to fast-forward") || e.contains("diverging branches") || e.contains("non-fast-forward") || e.contains("fetch first") {
        "the remote has commits this copy doesn't have, and this copy has its own: they can't be combined automatically. Resolve it with git on the command line.".into()
    } else if e.contains("Authentication failed") || e.contains("could not read Username") || e.contains("403") || e.contains("Permission denied") {
        format!("GitHub refused the sign-in: sign in with a token that has access to the repository ({e})")
    } else if e.contains("would be overwritten") {
        "the remote changes the document, which has changes that aren't committed: commit them first".into()
    } else {
        e.to_string()
    }
}

/// "owner/repo" for GitHub URLs, else the URL.
pub fn short_remote(url: &str) -> String {
    let u = url.trim_end_matches('/').trim_end_matches(".git");
    for pre in ["https://github.com/", "git@github.com:", "ssh://git@github.com/"] {
        if let Some(rest) = u.strip_prefix(pre) {
            return rest.to_string();
        }
    }
    u.to_string()
}

/// The page to open in a browser for a remote, when it is on GitHub.
pub fn web_url(url: &str) -> Option<String> {
    let u = url.trim();
    let gh = u.starts_with("https://github.com/") || u.starts_with("git@github.com:") || u.starts_with("ssh://git@github.com/");
    gh.then(|| format!("https://github.com/{}", short_remote(u)))
}

// ------------------------------------------------------------ comparing versions

/// What changed between two versions of a workspace, one line per tab, e.g.
/// "Part Studio 1: added Fillet 2; changed Extrude 1". Empty when nothing did.
pub fn compare(old: &Workspace, new: &Workspace) -> Vec<String> {
    let mut out = vec![];
    for s in &new.studios {
        let Some(o) = old.studios.iter().find(|o| o.name == s.name) else {
            out.push(format!("Added Part Studio “{}”", s.name));
            continue;
        };
        let mut what = vec![];
        let (od, nd) = (&o.doc, &s.doc);
        for f in &nd.features {
            match od.features.iter().find(|g| g.id == f.id) {
                None => what.push(format!("added {}", f.name)),
                Some(g) => {
                    if g.name != f.name {
                        what.push(format!("renamed {} to {}", g.name, f.name));
                    }
                    if g.suppressed != f.suppressed {
                        what.push(format!("{} {}", if f.suppressed { "suppressed" } else { "unsuppressed" }, f.name));
                    }
                    if serde_json::to_value(&g.op).ok() != serde_json::to_value(&f.op).ok() {
                        what.push(format!("changed {}", f.name));
                    }
                }
            }
        }
        for g in od.features.iter().filter(|g| !nd.features.iter().any(|f| f.id == g.id)) {
            what.push(format!("deleted {}", g.name));
        }
        for v in &nd.variables {
            match od.variables.iter().find(|w| w.name == v.name) {
                None => what.push(format!("added variable {} = {}", v.name, v.expr)),
                Some(w) if w.expr != v.expr => what.push(format!("{} {} → {}", v.name, w.expr, v.expr)),
                _ => {}
            }
        }
        for w in od.variables.iter().filter(|w| !nd.variables.iter().any(|v| v.name == w.name)) {
            what.push(format!("deleted variable {}", w.name));
        }
        if od.rollback != nd.rollback {
            what.push("moved the rollback bar".into());
        }
        let rest = |d: &crate::doc::Document| serde_json::to_value((&d.units, &d.parts, &d.configs, &d.active_config, &d.decals, &d.custom_features)).ok();
        if what.is_empty() && rest(od) != rest(nd) {
            what.push("changed part properties or settings".into());
        }
        if !what.is_empty() {
            out.push(format!("{}: {}", s.name, what.join("; ")));
        }
    }
    for o in old.studios.iter().filter(|o| !new.studios.iter().any(|s| s.name == o.name)) {
        out.push(format!("Deleted Part Studio “{}”", o.name));
    }
    fn tabs<T: serde::Serialize>(out: &mut Vec<String>, kind: &str, old: &[T], new: &[T], name: impl Fn(&T) -> &str) {
        for n in new {
            match old.iter().find(|o| name(o) == name(n)) {
                None => out.push(format!("Added {kind} “{}”", name(n))),
                Some(o) if serde_json::to_value(o).ok() != serde_json::to_value(n).ok() => out.push(format!("Changed {kind} “{}”", name(n))),
                _ => {}
            }
        }
        for o in old.iter().filter(|o| !new.iter().any(|n| name(n) == name(o))) {
            out.push(format!("Deleted {kind} “{}”", name(o)));
        }
    }
    tabs(&mut out, "Assembly", &old.assemblies, &new.assemblies, |t| &t.name);
    tabs(&mut out, "Drawing", &old.drawings, &new.drawings, |t| &t.name);
    tabs(&mut out, "Manufacture", &old.cams, &new.cams, |t| &t.name);
    tabs(&mut out, "Simulation", &old.sims, &new.sims, |t| &t.name);
    tabs(&mut out, "PCB", &old.pcbs, &new.pcbs, |t| &t.name);
    for v in new.versions.iter().filter(|v| !old.versions.iter().any(|o| o.name == v.name && o.created == v.created)) {
        out.push(format!("Created version “{}”", v.name));
    }
    out
}

/// A commit message from the list of changes: the first one as the subject (with a count of the
/// rest), and all of them in the body.
pub fn message(changes: &[String]) -> String {
    match changes {
        [] => "Save".into(),
        [one] => one.clone(),
        [first, rest @ ..] => {
            let mut subject = first.clone();
            if subject.chars().count() > 60 {
                subject = format!("{}…", subject.chars().take(59).collect::<String>());
            }
            format!("{subject} (+{} more)\n\n{}", rest.len(), changes.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"))
        }
    }
}

/// The commit message for saving `ws` over what the last commit has.
pub fn save_message(root: &Path, rel: &str, ws: &Workspace) -> String {
    match file_at(root, "HEAD", rel).ok().and_then(|s| Workspace::from_json(&s).ok()) {
        Some(old) => message(&compare(&old, ws)),
        None => "Save".into(),
    }
}

// ------------------------------------------------------------ GitHub

#[derive(Clone, Debug, PartialEq)]
pub struct GhUser {
    pub login: String,
    pub id: u64,
    pub name: Option<String>,
}

fn token_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cad-rust-app").join("github.json"))
}

/// Where the token comes from, for the account section.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TokenSource {
    Saved,
    Env,
    GhCli,
}

/// The GitHub token to use: the saved one, then `GITHUB_TOKEN`/`GH_TOKEN`, then the GitHub CLI's.
pub fn token() -> Option<(String, TokenSource)> {
    let saved = token_file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["token"].as_str().map(String::from));
    if let Some(t) = saved.filter(|t| !t.is_empty()) {
        return Some((t, TokenSource::Saved));
    }
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some((t.trim().to_string(), TokenSource::Env));
            }
        }
    }
    let mut c = Command::new("gh");
    c.args(["auth", "token"]).stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().ok().filter(|o| o.status.success())?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some((t, TokenSource::GhCli))
}

/// Save the token (readable only by the user); an empty one signs out.
pub fn save_token(token: &str) -> Result<(), String> {
    let p = token_file().ok_or("no config folder")?;
    if token.is_empty() {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    }
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(&json!({ "token": token })).unwrap();
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    use std::io::Write;
    o.open(&p).and_then(|mut f| f.write_all(body.as_bytes())).map_err(|e| e.to_string())
}

fn api_base() -> String {
    std::env::var("RCAD_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into()).trim_end_matches('/').to_string()
}

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(Duration::from_secs(60)).try_proxy_from_env(true).build()
}

fn gh_request(method: &str, path: &str, token: &str) -> ureq::Request {
    http()
        .request(method, &format!("{}{path}", api_base()))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", concat!("rcad/", env!("CARGO_PKG_VERSION")))
}

/// GitHub's error message for a failed call.
fn gh_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(401, _) => "GitHub didn't accept the token (it may have expired)".into(),
        ureq::Error::Status(code, r) => {
            let v: Value = r.into_string().ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
            let detail = v["errors"].as_array().and_then(|a| a.first()).and_then(|e| e["message"].as_str()).map(String::from);
            let msg = detail.or_else(|| v["message"].as_str().map(String::from)).unwrap_or_else(|| format!("HTTP {code}"));
            format!("GitHub: {msg}")
        }
        ureq::Error::Transport(t) => format!("could not reach GitHub: {t}"),
    }
}

fn json_of(r: ureq::Response) -> Result<Value, String> {
    r.into_string().ok().and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| "GitHub sent an unreadable answer".into())
}

/// The account a token belongs to.
pub fn gh_user(token: &str) -> Result<GhUser, String> {
    let v: Value = json_of(gh_request("GET", "/user", token).call().map_err(gh_error)?)?;
    let login = v["login"].as_str().ok_or("GitHub sent no user")?.to_string();
    Ok(GhUser { login, id: v["id"].as_u64().unwrap_or(0), name: v["name"].as_str().filter(|n| !n.is_empty()).map(String::from) })
}

/// Create a repository on the signed-in account; returns its clone URL.
pub fn gh_create_repo(token: &str, name: &str, private: bool, description: &str) -> Result<String, String> {
    let body = json!({ "name": name, "private": private, "description": description, "auto_init": false });
    let v: Value = json_of(gh_request("POST", "/user/repos", token).set("Content-Type", "application/json").send_string(&body.to_string()).map_err(gh_error)?)?;
    v["clone_url"].as_str().map(String::from).ok_or_else(|| "GitHub sent no repository URL".into())
}

/// A GitHub repository name from a document name: letters, digits, '-', '_' and '.'.
pub fn repo_name(stem: &str) -> String {
    let mut s: String = stem.trim().chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' }).collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() || s.chars().all(|c| c == '.') { "rcad-project".into() } else { s }
}

/// Publish: create the GitHub repository, make it `origin` and push to it.
pub fn publish(root: &Path, token: &str, name: &str, private: bool) -> Result<String, String> {
    let url = gh_create_repo(token, name, private, "Made with rCAD")?;
    set_remote(root, &url)?;
    push(root, Some(token))?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, Op};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rcad-vcs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn who() -> Identity {
        Identity { name: "Test".into(), email: "test@example.com".into() }
    }

    fn ws_with(features: &[&str]) -> Workspace {
        let mut doc = Document::default();
        for f in features {
            doc.insert(f.to_string(), Op::Plane { base: None, offset: "1".into(), mode: "offset".into(), refs: vec![], angle: "0".into(), flip: false });
        }
        let mut ws = Workspace::default();
        ws.studios[0].doc = doc;
        ws
    }

    #[test]
    fn track_commit_history_and_restore() {
        if version().is_none() {
            return;
        }
        let dir = scratch("track");
        let file = dir.join("bracket part.json");
        let ws1 = ws_with(&["Plane A"]);
        std::fs::write(&file, ws1.to_json()).unwrap();
        assert!(status(&file).is_none());
        let s = init(&file, &who()).unwrap();
        assert_eq!(s.branch, "main");
        assert!(s.autocommit && s.has_commits && !s.changed);
        assert_eq!(s.rel, "bracket part.json");

        // nothing changed: no commit
        assert!(commit(&s.root, &s.rel, "x", false, &who()).unwrap().is_none());

        let ws2 = ws_with(&["Plane A", "Plane B"]);
        std::fs::write(&file, ws2.to_json()).unwrap();
        std::fs::write(dir.join("notes.txt"), "hi").unwrap();
        let st = status(&file).unwrap();
        assert!(st.changed);
        assert_eq!(st.others, vec!["notes.txt".to_string()]);
        let msg = save_message(&st.root, &st.rel, &ws2);
        assert_eq!(msg, "Part Studio 1: added Plane B");
        let c = commit(&st.root, &st.rel, &msg, false, &who()).unwrap().unwrap();
        assert_eq!(c.subject, msg);
        // only the document was committed
        assert_eq!(status(&file).unwrap().others, vec!["notes.txt".to_string()]);

        let h = history(&st.root, &st.rel, 50);
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].subject, "Start tracking bracket part.json");
        let old = Workspace::from_json(&file_at(&st.root, &h[1].hash, &st.rel).unwrap()).unwrap();
        assert_eq!(old.studios[0].doc.features.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_and_pull_through_a_remote() {
        if version().is_none() {
            return;
        }
        let dir = scratch("remote");
        let bare = dir.join("remote.git");
        git(&dir, &["init", "-q", "--bare", bare.to_str().unwrap()]).unwrap();
        let a = dir.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let file = a.join("p.json");
        std::fs::write(&file, ws_with(&["P1"]).to_json()).unwrap();
        let s = init(&file, &who()).unwrap();
        set_remote(&s.root, bare.to_str().unwrap()).unwrap();
        push(&s.root, None).unwrap();
        let s = status(&file).unwrap();
        assert!(s.upstream && s.ahead == 0 && s.behind == 0);

        // a second copy commits; the first pulls it
        let b = dir.join("b");
        git(&dir, &["clone", "-q", "-b", "main", bare.to_str().unwrap(), b.to_str().unwrap()]).unwrap();
        let fb = b.join("p.json");
        std::fs::write(&fb, ws_with(&["P1", "P2"]).to_json()).unwrap();
        let sb = status(&fb).unwrap();
        commit(&sb.root, &sb.rel, "two planes", false, &who()).unwrap().unwrap();
        push(&sb.root, None).unwrap();
        assert_eq!(pull(&s.root, None).unwrap(), "Pulled the latest main from ".to_string() + dir.join("remote").to_str().unwrap());
        let ws = Workspace::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(ws.studios[0].doc.features.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accented_names_and_untracked_folders() {
        if version().is_none() {
            return;
        }
        let dir = scratch("accents");
        let file = dir.join("Halterung ü.json");
        std::fs::write(&file, ws_with(&["A"]).to_json()).unwrap();
        let s = init(&file, &who()).unwrap();
        // git quotes such names in its listings unless asked not to
        let ws2 = ws_with(&["A", "B"]);
        assert_eq!(save_message(&s.root, &s.rel, &ws2), "Part Studio 1: added B");
        assert!(Workspace::from_json(&file_at(&s.root, "HEAD", &s.rel).unwrap()).is_ok());
        // a new document in a new folder: git lists only the folder unless asked for every file
        let sub = dir.join("sub").join("Teil.json");
        std::fs::create_dir_all(sub.parent().unwrap()).unwrap();
        std::fs::write(&sub, ws2.to_json()).unwrap();
        assert!(status(&sub).unwrap().changed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_folder_in_another_repository_gets_its_own() {
        if version().is_none() {
            return;
        }
        let dir = scratch("outer");
        git(&dir, &["init", "-q"]).unwrap();
        let file = dir.join("Arm").join("Arm.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, ws_with(&["A"]).to_json()).unwrap();
        let s = init(&file, &who()).unwrap();
        assert_eq!(s.root, std::fs::canonicalize(file.parent().unwrap()).unwrap());
        assert_eq!(s.rel, "Arm.json");
        // the outer repository has no commits of it
        assert!(!git_ok(&dir, &["rev-parse", "-q", "--verify", "HEAD"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imported_config_keeps_only_safe_settings() {
        if version().is_none() {
            return;
        }
        let dir = scratch("cfg");
        let cfg = b"[core]\n\trepositoryformatversion = 0\n\tfsmonitor = touch PWNED\n\thooksPath = /tmp\n[remote \"origin\"]\n\turl = https://github.com/a/b.git\n\tuploadpack = touch PWNED\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[filter \"x\"]\n\tclean = touch PWNED\n";
        write_imported_config(&dir, Some(cfg)).unwrap();
        let out = std::fs::read_to_string(dir.join("config")).unwrap();
        assert!(!out.contains("PWNED") && !out.contains("hooksPath"), "{out}");
        assert!(out.contains("https://github.com/a/b.git") && out.contains("refs/heads/main"), "{out}");
        assert!(!dir.join("config.imported").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_lists_changes() {
        let old = ws_with(&["A", "B"]);
        let mut new = old.clone();
        new.studios[0].doc.features[0].name = "A2".into();
        new.studios[0].doc.features.remove(1);
        new.studios[0].doc.variables.push(crate::doc::Variable { name: "w".into(), expr: "10".into() });
        new.studios.push(crate::workspace::Studio { name: "Part Studio 2".into(), doc: Document::default() });
        new.make_version("v1".into());
        let c = compare(&old, &new);
        assert_eq!(c, vec!["Part Studio 1: renamed A to A2; deleted B; added variable w = 10", "Added Part Studio “Part Studio 2”", "Created version “v1”"]);
        assert!(compare(&old, &old).is_empty());
        assert!(message(&c).starts_with("Part Studio 1: renamed A to A2; deleted B; added variable w = 10 (+2 more)".chars().take(59).collect::<String>().as_str()));
    }

    #[test]
    fn names_and_urls() {
        assert_eq!(repo_name("My bracket (v2)"), "My-bracket-v2");
        assert_eq!(repo_name("  "), "rcad-project");
        assert_eq!(short_remote("https://github.com/jo/arm.git"), "jo/arm");
        assert_eq!(web_url("git@github.com:jo/arm.git").as_deref(), Some("https://github.com/jo/arm"));
        assert_eq!(web_url("/srv/git/arm.git"), None);
        assert!(auth_env(Some("t"), "https://gitlab.com/x").is_empty());
        assert_eq!(auth_env(Some("t"), "https://github.com/x/y.git").len(), 3);
    }
}
