//! Documents kept on another computer on the network (netdash's rCAD receiver, the same service
//! the phone app talks to: `capture/receiver/rcad_projects.py`), opened and saved from here.
//!
//! Pairing stores the receiver's address and token in `~/.config/cad-rust-app/remote.json`
//! (token encrypted for the user on Windows, the file readable only by them elsewhere). Opening
//! a remote document downloads it, with its chats and build cache entries, into a working copy in
//! the user cache folder (`<cache>/cad-rust-app/remote/<host>/<root>/<path>`), and the editor opens
//! that copy like any other file. Beside it a hidden link file (`.<name>.rcad-remote`) remembers
//! which remote document it is and the etag (sha256) of the copy it was made from.
//!
//! Saving writes the working copy, then sends it back with `If-Match: <that etag>`: the receiver
//! refuses a save made against a copy that has changed there since (a phone chat, another
//! computer), and the editor offers to reload theirs, keep mine, or save mine as a copy. A
//! working copy whose last save never reached the receiver (no network) is kept as it is when the
//! document is opened again, so the next save sends it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Biggest file downloaded from the build cache (bigger entries are rebuilt here instead).
const MAX_CACHE_ENTRY: u64 = 64 << 20;

// ---------------------------------------------------------------- pairing

/// A paired computer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Server {
    /// Addresses to try in order (`http://192.168.0.247:8765`, then e.g. a WireGuard one).
    pub urls: Vec<String>,
    pub token: String,
    /// Its host name, as it reported it.
    #[serde(default)]
    pub host: String,
}

impl Server {
    pub fn label(&self) -> String {
        if !self.host.is_empty() { self.host.clone() } else { self.urls.first().map(|u| host_of(u)).unwrap_or_else(|| "remote computer".into()) }
    }
}

fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', ':']).next().unwrap_or(rest).to_string()
}

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cad-rust-app"))
}

fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("remote.json"))
}

/// The paired computer, if any.
pub fn load_server() -> Option<Server> {
    let text = std::fs::read_to_string(config_file()?).ok()?;
    server_from(&text)
}

fn server_from(text: &str) -> Option<Server> {
    let v: Value = serde_json::from_str(text).ok()?;
    let urls: Vec<String> = v.get("urls")?.as_array()?.iter().filter_map(|u| u.as_str().map(String::from)).collect();
    let token = if let Some(p) = v.get("protected").and_then(|p| p.as_str()) {
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD.decode(p).ok()?;
        String::from_utf8(crate::vcs::protect::unprotect(&raw)?).ok()?
    } else {
        v.get("token")?.as_str()?.to_string()
    };
    (!urls.is_empty() && !token.is_empty()).then(|| Server { urls, token, host: v.get("host").and_then(|h| h.as_str()).unwrap_or("").to_string() })
}

/// Remember the paired computer (None: forget it).
pub fn save_server(s: Option<&Server>) -> Result<(), String> {
    let p = config_file().ok_or("no config folder")?;
    let Some(s) = s else {
        let _ = std::fs::remove_file(&p);
        return Ok(());
    };
    let mut v = json!({"urls": s.urls, "host": s.host});
    match crate::vcs::protect::protect(s.token.as_bytes()) {
        Some(b) => {
            use base64::Engine;
            v["protected"] = json!(base64::engine::general_purpose::STANDARD.encode(b));
        }
        None => v["token"] = json!(s.token),
    }
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    use std::io::Write;
    o.open(&p).and_then(|mut f| f.write_all(serde_json::to_string_pretty(&v).unwrap().as_bytes())).map_err(|e| e.to_string())
}

/// The addresses and token in a pairing link: `rcad://pair?url=<a>,<b>&token=<t>` (also
/// `rcadcapture://…`, what the receiver's pairing page gives) or the page's own address
/// `http://host:8765/pair?t=<t>`.
pub fn parse_pair(link: &str) -> Result<(Vec<String>, String), String> {
    let link = link.trim();
    let (head, query) = link.split_once('?').ok_or("that isn't a pairing link: it has no ?url=…&token=…")?;
    let mut urls = vec![];
    let mut token = String::new();
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = percent_decode(v);
        match k {
            "url" => urls.extend(v.split(',').map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty())),
            "token" | "t" => token = v,
            _ => {}
        }
    }
    if urls.is_empty() && (head.starts_with("http://") || head.starts_with("https://")) {
        // the pairing page itself: its own address
        urls.push(head.trim_end_matches("/pair").trim_end_matches('/').to_string());
    }
    if let Some(bad) = urls.iter().find(|u| !(u.starts_with("http://") || u.starts_with("https://"))) {
        return Err(format!("{bad} isn't an http address"));
    }
    if urls.is_empty() || token.is_empty() {
        return Err("the link has no address or no token".into());
    }
    Ok((urls, token))
}

/// An address typed in: `192.168.0.247`, `netdash:8765`, `http://…`; port 8765 when none is given.
pub fn normalize_url(typed: &str) -> String {
    let t = typed.trim().trim_end_matches('/');
    let with_scheme = if t.contains("://") { t.to_string() } else { format!("http://{t}") };
    let after = with_scheme.split("://").nth(1).unwrap_or("");
    if after.contains(':') { with_scheme } else { format!("{with_scheme}:8765") }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    s.bytes().map(|c| if c.is_ascii_alphanumeric() || b"-._~".contains(&c) { (c as char).to_string() } else { format!("%{c:02X}") }).collect()
}

// ---------------------------------------------------------------- the client

/// A document on the remote computer, as its list gives it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct RemoteDoc {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub project: String,
    /// Its path on the remote computer.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub modified: u64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub git: bool,
    /// Claude is working on it there (from the phone).
    #[serde(default)]
    pub busy: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RemoteFile {
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub etag: Option<String>,
}

/// How a save went wrong.
#[derive(Clone, Debug, PartialEq)]
pub enum SaveError {
    /// The remote copy changed since this one was made from it; its etag now.
    Stale(Option<String>),
    /// Claude is working on it over there.
    Busy(String),
    Other(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Stale(_) => write!(f, "the copy on the other computer changed since this one was opened"),
            SaveError::Busy(e) | SaveError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub struct Client {
    agent: ureq::Agent,
    pub base: String,
    token: String,
    /// The remote computer's host name.
    pub host: String,
}

fn http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let text = r.into_string().unwrap_or_default();
            serde_json::from_str::<Value>(&text).ok().and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from)).unwrap_or(format!("the receiver answered {code}"))
        }
        ureq::Error::Transport(t) => format!("can't reach it: {t}"),
    }
}

fn json_of(r: ureq::Response) -> Result<Value, String> {
    let text = r.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("the receiver's answer isn't JSON: {e}"))
}

fn this_computer() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| std::fs::read_to_string("/etc/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "another computer".into()))
}

impl Client {
    /// Connect to the first of the server's addresses that answers with the right token.
    pub fn connect(s: &Server) -> Result<Client, String> {
        let mut last = "no address".to_string();
        for url in &s.urls {
            // no proxy: the receiver is on the local network
            let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(4)).timeout_read(Duration::from_secs(120)).timeout_write(Duration::from_secs(300)).build();
            let mut c = Client { agent, base: url.trim_end_matches('/').to_string(), token: s.token.clone(), host: String::new() };
            match c.get_json("/api/ping") {
                Ok(v) if v.get("files").and_then(|f| f.as_bool()) == Some(true) => {
                    c.host = v.get("host").and_then(|h| h.as_str()).unwrap_or("").to_string();
                    return Ok(c);
                }
                Ok(_) => last = format!("{url} runs an older receiver that can't open documents: update it"),
                Err(e) => last = format!("{url}: {e}"),
            }
        }
        Err(last)
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        self.agent.request(method, &format!("{}{path}", self.base)).set("Authorization", &format!("Bearer {}", self.token))
    }

    fn get_json(&self, path: &str) -> Result<Value, String> {
        json_of(self.req("GET", path).call().map_err(http_error)?)
    }

    /// Every document in the remote computer's project folders, newest first.
    pub fn documents(&self) -> Result<Vec<RemoteDoc>, String> {
        let v = self.get_json("/api/projects")?;
        serde_json::from_value(v.get("documents").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())
    }

    /// The document's bytes and etag, or None when it is still `if_none_match`.
    pub fn content(&self, id: &str, if_none_match: Option<&str>) -> Result<Option<(Vec<u8>, String)>, String> {
        let mut r = self.req("GET", &format!("/api/doc/{id}/content"));
        if let Some(t) = if_none_match {
            r = r.set("If-None-Match", &format!("\"{t}\""));
        }
        let resp = r.call().map_err(http_error)?;
        if resp.status() == 304 {
            return Ok(None);
        }
        let tag = resp.header("ETag").map(|t| t.trim_matches('"').to_string()).unwrap_or_default();
        let mut bytes = vec![];
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes).map_err(|e| e.to_string())?;
        let tag = if tag.is_empty() { sha256_hex(&bytes) } else { tag };
        Ok(Some((bytes, tag)))
    }

    /// Send `bytes` as the document (or, with `rel`, one of its files). `base` is the etag of the
    /// copy it was made from; `"*"` replaces whatever is there, `""` only makes a new file.
    pub fn put(&self, id: &str, rel: Option<&str>, bytes: &[u8], base: &str) -> Result<String, SaveError> {
        let path = match rel {
            None => format!("/api/doc/{id}/content"),
            Some(r) => format!("/api/doc/{id}/files/{r}"),
        };
        let mut r = self.req("PUT", &path).set("X-Rcad-Client", &this_computer()).set("Content-Type", "application/json");
        r = match base {
            "" => r.set("If-None-Match", "*"),
            "*" => r.set("If-Match", "*"),
            t => r.set("If-Match", &format!("\"{t}\"")),
        };
        match r.send_bytes(bytes) {
            Ok(resp) => {
                let v = json_of(resp).map_err(SaveError::Other)?;
                Ok(v.get("etag").and_then(|t| t.as_str()).map(String::from).unwrap_or_else(|| sha256_hex(bytes)))
            }
            Err(ureq::Error::Status(412, r)) => {
                let v = json_of(r).unwrap_or(json!({}));
                Err(SaveError::Stale(v.get("etag").and_then(|t| t.as_str()).map(String::from)))
            }
            Err(ureq::Error::Status(409, r)) => {
                let v = json_of(r).unwrap_or(json!({}));
                Err(SaveError::Busy(v.get("error").and_then(|t| t.as_str()).unwrap_or("it is busy over there").to_string()))
            }
            Err(e) => Err(SaveError::Other(http_error(e))),
        }
    }

    /// Save `bytes` as a new document `name` beside the document `id`: (its id, etag, file name).
    pub fn put_copy(&self, id: &str, name: &str, bytes: &[u8]) -> Result<(String, String, String), String> {
        let resp = self
            .req("PUT", &format!("/api/doc/{id}/content?copy={}", percent_encode(name)))
            .set("X-Rcad-Client", &this_computer())
            .set("Content-Type", "application/json")
            .send_bytes(bytes)
            .map_err(http_error)?;
        let v = json_of(resp)?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        Ok((s("id"), s("etag"), s("name")))
    }

    pub fn files(&self, id: &str) -> Result<Vec<RemoteFile>, String> {
        let v = self.get_json(&format!("/api/doc/{id}/files"))?;
        serde_json::from_value(v.get("files").cloned().unwrap_or(json!([]))).map_err(|e| e.to_string())
    }

    pub fn file(&self, id: &str, rel: &str) -> Result<Vec<u8>, String> {
        let resp = self.req("GET", &format!("/api/doc/{id}/files/{rel}")).call().map_err(http_error)?;
        let mut bytes = vec![];
        std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}

pub fn sha256_hex(b: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, b).as_ref().iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------- working copies

/// What a working copy is a copy of (its hidden link file beside it).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Link {
    /// The remote computer's host name and the document's id there.
    pub host: String,
    pub id: String,
    /// Its path over there, for showing.
    #[serde(default)]
    pub remote_path: String,
    /// The etag of the remote copy this one was last opened from or saved to.
    pub etag: String,
    /// The sha256 of this copy as it was then: when the file differs, it has a save that never
    /// reached the other computer.
    pub synced: String,
    /// The etags of the side files (`.rcad-chats/…`) as last synced, by path.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

fn link_file(doc: &Path) -> PathBuf {
    let name = doc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    doc.with_file_name(format!(".{name}.rcad-remote"))
}

/// The link of a working copy, or None for an ordinary document.
pub fn link_for(doc: &Path) -> Option<Link> {
    serde_json::from_str(&std::fs::read_to_string(link_file(doc)).ok()?).ok()
}

pub fn write_link(doc: &Path, l: &Link) -> Result<(), String> {
    crate::session::write_atomic(&link_file(doc), serde_json::to_string_pretty(l).unwrap().as_bytes()).map_err(|e| e.to_string())
}

/// The folder working copies of `host`'s documents go in.
pub fn working_dir(host: &str) -> Option<PathBuf> {
    let base = std::env::var_os("RCAD_REMOTE_DIR").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| crate::build_cache::user_dir()?.parent().map(|d| d.join("remote")))?;
    let safe: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
    Some(base.join(if safe.trim_matches('.').is_empty() { "remote".into() } else { safe }))
}

/// The path inside a document id (`<root>-<base64url path>`), as safe relative components:
/// `<root>/<folders>/<name>.json`. None for an id that isn't one or would leave the folder.
pub fn id_path(id: &str) -> Option<PathBuf> {
    use base64::Engine;
    let (root, enc) = id.split_once('-')?;
    if root.is_empty() || !root.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(enc.trim_end_matches('=')).ok()?;
    let rel = String::from_utf8(raw).ok()?;
    let mut out = PathBuf::from(root);
    let parts: Vec<&str> = rel.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() || !parts.last()?.ends_with(".json") {
        return None;
    }
    for p in parts {
        // nothing that climbs out, names a drive or is hidden
        if p == ".." || p.contains(':') || p.starts_with('.') || p.chars().any(|c| c.is_control() || "<>\"|?*".contains(c)) {
            return None;
        }
        out.push(p);
    }
    Some(out)
}

/// Whether a side file path from the server is one to keep: `.rcad-chats/<name>.json` or
/// `.rcad-cache/<hex>.json.gz`, nothing else.
fn side_ok(rel: &str) -> bool {
    let Some((dir, name)) = rel.split_once('/') else { return false };
    let plain = !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    plain && ((dir == crate::claude_chats::DIR && name.ends_with(".json")) || (dir == crate::build_cache::DIR && name.ends_with(".json.gz")))
}

/// What opening a remote document did.
#[derive(Debug)]
pub struct Opened {
    pub local: PathBuf,
    /// The working copy had a save that never reached the other computer: it was kept, not
    /// replaced (the next save sends it).
    pub kept_local: bool,
    /// Chats and build cache entries copied over.
    pub files: usize,
}

/// Download a document (and its chats and build cache) into its working copy. A working copy
/// with a save the other computer never got is kept as it is.
pub fn open(c: &Client, doc: &RemoteDoc) -> Result<Opened, String> {
    let dir = working_dir(&c.host).ok_or("no cache folder for the working copy")?;
    let rel = id_path(&doc.id).ok_or("the document id isn't one this version understands")?;
    let local = dir.join(rel);
    let old = link_for(&local).filter(|l| l.id == doc.id);
    if let Some(l) = &old {
        if local.is_file() && std::fs::read(&local).map(|b| sha256_hex(&b)).ok().as_deref() != Some(l.synced.as_str()) {
            return Ok(Opened { local, kept_local: true, files: 0 });
        }
    }
    let (bytes, etag) = c.content(&doc.id, None)?.ok_or("the receiver sent nothing")?;
    std::fs::create_dir_all(local.parent().unwrap()).map_err(|e| format!("can't make {}: {e}", local.parent().unwrap().display()))?;
    let mut link = Link {
        host: c.host.clone(),
        id: doc.id.clone(),
        remote_path: doc.path.clone(),
        etag: etag.clone(),
        synced: sha256_hex(&bytes),
        files: old.map(|l| l.files).unwrap_or_default(),
    };
    let files = pull_files(c, &local, &mut link).unwrap_or(0);
    crate::session::write_atomic(&local, &bytes).map_err(|e| format!("can't write {}: {e}", local.display()))?;
    write_link(&local, &link)?;
    Ok(Opened { local, kept_local: false, files })
}

/// Copy the document's chats (newer than here) and build cache entries (missing here) into the
/// working copy's folder. How many were copied.
fn pull_files(c: &Client, local: &Path, link: &mut Link) -> Result<usize, String> {
    let folder = local.parent().unwrap();
    let mut n = 0;
    for f in c.files(&link.id)? {
        if !side_ok(&f.path) {
            continue;
        }
        let dest = folder.join(&f.path);
        let is_cache = f.path.starts_with(crate::build_cache::DIR);
        if is_cache && (dest.is_file() || f.size > MAX_CACHE_ENTRY) {
            continue;
        }
        let here = std::fs::read(&dest).ok().map(|b| sha256_hex(&b));
        if !is_cache && here.is_some() && here == f.etag {
            link.files.insert(f.path.clone(), here.unwrap());
            continue;
        }
        // a chat changed here and not saved back yet stays as it is
        if !is_cache && here.is_some() && link.files.get(&f.path) != here.as_ref() {
            continue;
        }
        let bytes = c.file(&link.id, &f.path)?;
        std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        crate::session::write_atomic(&dest, &bytes).map_err(|e| e.to_string())?;
        if !is_cache {
            link.files.insert(f.path.clone(), sha256_hex(&bytes));
        }
        n += 1;
    }
    Ok(n)
}

/// What a save back did.
#[derive(Debug, Default)]
pub struct Pushed {
    pub etag: String,
    /// Chat files sent.
    pub chats: usize,
    /// Chat files the other computer changed meanwhile (kept as they are there).
    pub chat_conflicts: usize,
}

/// Send a saved working copy back. `base` overrides the etag it was made from (`"*"`: replace
/// whatever is there, after the user chose to keep theirs). The link is updated on success.
pub fn push(c: &Client, local: &Path, base: Option<&str>) -> Result<Pushed, SaveError> {
    let mut link = link_for(local).ok_or_else(|| SaveError::Other("this isn't a copy of a remote document".into()))?;
    let bytes = std::fs::read(local).map_err(|e| SaveError::Other(format!("can't read {}: {e}", local.display())))?;
    let etag = c.put(&link.id, None, &bytes, base.unwrap_or(&link.etag))?;
    link.etag = etag.clone();
    link.synced = sha256_hex(&bytes);
    let mut out = Pushed { etag, ..Default::default() };
    // the chats of this document that changed here
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let chats = local.parent().unwrap().join(crate::claude_chats::DIR);
    let ours: Vec<String> = crate::claude_chats::list(local).into_iter().map(|m| m.id).collect();
    for id in ours {
        for file in [format!("{id}.context.json"), format!("{id}.json")] {
            let rel = format!("{}/{file}", crate::claude_chats::DIR);
            let Ok(b) = std::fs::read(chats.join(&file)) else { continue };
            let h = sha256_hex(&b);
            let was = link.files.get(&rel).cloned();
            if was.as_deref() == Some(h.as_str()) {
                continue;
            }
            match c.put(&link.id, Some(&rel), &b, was.as_deref().unwrap_or("")) {
                Ok(t) => {
                    link.files.insert(rel, t);
                    out.chats += 1;
                }
                Err(SaveError::Stale(_)) => out.chat_conflicts += 1,
                Err(e) => log::warn!("could not send {rel} of {name}: {e}"),
            }
        }
    }
    let _ = write_link(local, &link);
    Ok(out)
}

/// Replace the working copy with the remote document as it is now (discarding what is here).
pub fn reload(c: &Client, local: &Path) -> Result<(), String> {
    let mut link = link_for(local).ok_or("this isn't a copy of a remote document")?;
    let (bytes, etag) = c.content(&link.id, None)?.ok_or("the receiver sent nothing")?;
    let _ = pull_files(c, local, &mut link);
    crate::session::write_atomic(local, &bytes).map_err(|e| e.to_string())?;
    link.etag = etag;
    link.synced = sha256_hex(&bytes);
    write_link(local, &link)
}

/// Save the working copy as a new remote document `name` beside its own, which this copy then
/// stops following: the new document's working copy (the same content), to open.
pub fn push_copy(c: &Client, local: &Path, name: &str) -> Result<PathBuf, String> {
    let link = link_for(local).ok_or("this isn't a copy of a remote document")?;
    let bytes = std::fs::read(local).map_err(|e| e.to_string())?;
    let (id, etag, _) = c.put_copy(&link.id, name, &bytes)?;
    let rel = id_path(&id).ok_or("the receiver gave a strange id")?;
    let new_local = working_dir(&c.host).ok_or("no cache folder")?.join(rel);
    std::fs::create_dir_all(new_local.parent().unwrap()).map_err(|e| e.to_string())?;
    crate::session::write_atomic(&new_local, &bytes).map_err(|e| e.to_string())?;
    let remote_path = Path::new(&link.remote_path).with_file_name(new_local.file_name().unwrap()).display().to_string();
    write_link(&new_local, &Link { host: c.host.clone(), id, remote_path, etag, synced: sha256_hex(&bytes), files: BTreeMap::new() })?;
    // the old copy re-downloads next time instead of keeping this content as unsent changes
    let _ = std::fs::remove_file(link_file(local));
    let _ = std::fs::remove_file(local);
    Ok(new_local)
}

/// Whether the remote document changed from the copy this was made from: its bytes and etag then.
pub fn check(c: &Client, link: &Link) -> Result<Option<String>, String> {
    Ok(c.content(&link.id, Some(&link.etag))?.map(|(_, t)| t))
}

/// Find a remote document by its id, its path over there, or `<project>/<file>` as listed.
pub fn find<'a>(docs: &'a [RemoteDoc], what: &str) -> Option<&'a RemoteDoc> {
    let w = what.trim().trim_start_matches('/').replace('\\', "/");
    docs.iter().find(|d| d.id == w || d.path == what.trim()).or_else(|| {
        docs.iter().find(|d| {
            let rel = id_path(&d.id).map(|p| p.iter().skip(1).map(|c| c.to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")).unwrap_or_default();
            rel == w || rel.trim_end_matches(".json") == w || d.path.ends_with(&format!("/{w}"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[test]
    fn pairing_links() {
        let (u, t) = parse_pair("rcad://pair?url=http%3A%2F%2F192.168.0.247%3A8765%2Chttp%3A%2F%2F10.8.0.1%3A8765&token=abc_DEF-1").unwrap();
        assert_eq!(u, vec!["http://192.168.0.247:8765", "http://10.8.0.1:8765"]);
        assert_eq!(t, "abc_DEF-1");
        let (u, t) = parse_pair("rcadcapture://pair?url=http://netdash:8765/&token=x").unwrap();
        assert_eq!((u, t.as_str()), (vec!["http://netdash:8765".to_string()], "x"));
        let (u, t) = parse_pair(" http://192.168.0.247:8765/pair?t=tok ").unwrap();
        assert_eq!((u, t.as_str()), (vec!["http://192.168.0.247:8765".to_string()], "tok"));
        assert!(parse_pair("rcad://pair?url=file:///etc&token=x").is_err());
        assert!(parse_pair("rcad://pair?url=http://a").is_err());
        assert!(parse_pair("hello").is_err());
        assert_eq!(normalize_url("192.168.0.247"), "http://192.168.0.247:8765");
        assert_eq!(normalize_url("http://netdash:9000/"), "http://netdash:9000");
        assert_eq!(percent_decode("a%2Fb%zz+c%"), "a/b%zz c%");
    }

    #[test]
    fn ids_become_safe_paths() {
        use base64::Engine;
        let id = |root: &str, p: &str| format!("{root}-{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(p));
        assert_eq!(id_path(&id("0", "bracket-example/bracket.json")), Some(PathBuf::from("0").join("bracket-example").join("bracket.json")));
        assert_eq!(id_path(&id("1", "a.json")), Some(PathBuf::from("1").join("a.json")));
        for bad in ["../x.json", "a/../../x.json", "C:/x.json", "a/.git/x.json", "a/b.txt", "", "/"] {
            assert_eq!(id_path(&id("0", bad)), None, "{bad}");
        }
        assert_eq!(id_path("x-YQ"), None);
        assert_eq!(id_path("0-!!"), None);
        assert!(side_ok(".rcad-chats/1700000000-ab12.json") && side_ok(".rcad-chats/c1.context.json") && side_ok(".rcad-cache/00000000000000ab.json.gz"));
        for bad in [".rcad-chats/../x.json", ".git/config", ".rcad-chats/.gitignore", ".rcad-cache/a.json", ".rcad-chats/a/b.json", "x.json"] {
            assert!(!side_ok(bad), "{bad}");
        }
    }

    #[test]
    fn server_round_trip() {
        let v = json!({"urls": ["http://a:1"], "token": "t", "host": "netdash"}).to_string();
        assert_eq!(server_from(&v), Some(Server { urls: vec!["http://a:1".into()], token: "t".into(), host: "netdash".into() }));
        assert_eq!(server_from("{}"), None);
    }

    /// A stand-in for the receiver: one document and its chats, with etags and If-Match like
    /// `rcad_projects.py`.
    struct Fake {
        files: BTreeMap<String, Vec<u8>>,
        puts: Vec<(String, String)>,
    }

    fn serve(state: Arc<Mutex<Fake>>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let st = state.clone();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut it = line.split_whitespace();
                        let (method, path) = (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
                        let mut h = BTreeMap::new();
                        loop {
                            let mut l = String::new();
                            r.read_line(&mut l).unwrap();
                            if l.trim().is_empty() {
                                break;
                            }
                            let (k, v) = l.split_once(':').unwrap();
                            h.insert(k.trim().to_lowercase(), v.trim().to_string());
                        }
                        let n: usize = h.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let mut body = vec![0; n];
                        r.read_exact(&mut body).unwrap();
                        let (code, extra, out) = handle(&st, &method, &path, &h, body);
                        let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Length: {}\r\n{extra}\r\n", out.len());
                        let _ = s.write_all(&out);
                    }
                });
            }
        });
        addr
    }

    fn handle(st: &Arc<Mutex<Fake>>, method: &str, path: &str, h: &BTreeMap<String, String>, body: Vec<u8>) -> (u16, String, Vec<u8>) {
        let mut st = st.lock().unwrap();
        if h.get("authorization").map(String::as_str) != Some("Bearer tok") {
            return (401, String::new(), b"{\"error\": \"bad or missing token\"}".to_vec());
        }
        let id = "0-YnJhY2tldC9icmFja2V0Lmpzb24"; // bracket/bracket.json
        let key = match path {
            "/api/ping" => return (200, String::new(), br#"{"ok": true, "host": "netdash", "files": true}"#.to_vec()),
            "/api/projects" => {
                return (
                    200,
                    String::new(),
                    json!({"documents": [{"id": id, "name": "bracket", "project": "bracket", "path": "/home/j/rCAD Projects/bracket/bracket.json"}]}).to_string().into_bytes(),
                );
            }
            p if p == format!("/api/doc/{id}/files") => {
                let list: Vec<Value> = st.files.iter().filter(|(k, _)| k.starts_with('.')).map(|(k, v)| json!({"path": k, "size": v.len(), "etag": sha256_hex(v)})).collect();
                return (200, String::new(), json!({"files": list}).to_string().into_bytes());
            }
            p if p == format!("/api/doc/{id}/content") => "doc".to_string(),
            p if p.starts_with(&format!("/api/doc/{id}/content?copy=")) => {
                st.files.insert("copy".into(), body.clone());
                st.puts.push(("copy".into(), p.rsplit('=').next().unwrap().into()));
                return (200, String::new(), json!({"id": "0-YnJhY2tldC9jb3B5Lmpzb24", "etag": sha256_hex(&body), "name": "copy.json"}).to_string().into_bytes());
            }
            p if p.starts_with(&format!("/api/doc/{id}/files/")) => p.rsplit_once(&format!("/api/doc/{id}/files/")).unwrap().1.to_string(),
            _ => return (404, String::new(), b"{}".to_vec()),
        };
        let now = st.files.get(&key).map(|b| sha256_hex(b));
        if method == "GET" {
            let Some(b) = st.files.get(&key) else { return (404, String::new(), b"{}".to_vec()) };
            let tag = now.unwrap();
            if h.get("if-none-match").map(|t| t.trim_matches('"')) == Some(tag.as_str()) {
                return (304, format!("ETag: \"{tag}\"\r\n"), vec![]);
            }
            return (200, format!("ETag: \"{tag}\"\r\n"), b.clone());
        }
        let want = if h.get("if-none-match").map(String::as_str) == Some("*") { Some("".to_string()) } else { h.get("if-match").map(|t| t.trim_matches('"').to_string()) };
        let ok = match (want.as_deref(), &now) {
            (Some(""), None) | (Some("*"), Some(_)) => true,
            (Some(w), Some(n)) => w == n,
            _ => false,
        };
        if !ok {
            return (412, String::new(), json!({"error": "changed", "etag": now}).to_string().into_bytes());
        }
        st.puts.push((key.clone(), want.unwrap()));
        st.files.insert(key, body.clone());
        (200, String::new(), json!({"ok": true, "etag": sha256_hex(&body)}).to_string().into_bytes())
    }

    #[test]
    fn open_save_and_conflicts_against_a_fake_receiver() {
        let tmp = std::env::temp_dir().join(format!("rcad-remote-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: the only test that sets it
        unsafe { std::env::set_var("RCAD_REMOTE_DIR", &tmp) };
        let doc = br#"{"format": "cad-rust-app/workspace-1", "studios": []}"#.to_vec();
        let chat = json!({"format": 1, "id": "c1", "document": "bracket.json", "title": "t", "created": 1, "updated": 2, "model": "", "lines": []}).to_string().into_bytes();
        let mut files = BTreeMap::new();
        files.insert("doc".to_string(), doc.clone());
        files.insert(".rcad-chats/c1.json".to_string(), chat.clone());
        files.insert(".rcad-cache/00000000000000ab.json.gz".to_string(), b"gz".to_vec());
        let st = Arc::new(Mutex::new(Fake { files, puts: vec![] }));
        let url = serve(st.clone());
        assert!(Client::connect(&Server { urls: vec![url.clone()], token: "nope".into(), host: String::new() }).is_err());
        let c = Client::connect(&Server { urls: vec!["http://127.0.0.1:1".into(), url.clone()], token: "tok".into(), host: String::new() }).unwrap();
        assert_eq!(c.host, "netdash");
        let docs = c.documents().unwrap();
        assert!(find(&docs, "bracket/bracket.json").is_some() && find(&docs, "bracket/bracket").is_some() && find(&docs, "nope").is_none());

        // open: the document, its chat and its cache entry land in the working copy
        let o = open(&c, &docs[0]).unwrap();
        assert_eq!(o.local, tmp.join("netdash").join("0").join("bracket").join("bracket.json"));
        assert_eq!(std::fs::read(&o.local).unwrap(), doc);
        assert_eq!(o.files, 2);
        let folder = o.local.parent().unwrap();
        assert_eq!(std::fs::read(folder.join(".rcad-chats/c1.json")).unwrap(), chat);
        assert!(folder.join(".rcad-cache/00000000000000ab.json.gz").is_file());
        let link = link_for(&o.local).unwrap();
        assert_eq!(link.etag, sha256_hex(&doc));
        assert_eq!(check(&c, &link).unwrap(), None);

        // save: sent with the etag it was opened from
        let mine = br#"{"format": "cad-rust-app/workspace-1", "studios": [{"name": "Mine"}]}"#.to_vec();
        std::fs::write(&o.local, &mine).unwrap();
        let p = push(&c, &o.local, None).unwrap();
        assert_eq!(p.etag, sha256_hex(&mine));
        assert_eq!(st.lock().unwrap().files["doc"], mine);
        assert_eq!(st.lock().unwrap().puts.last().unwrap(), &("doc".to_string(), sha256_hex(&doc)));

        // the other computer changes it: the check notices, a save is refused and nothing is lost
        let theirs = br#"{"format": "cad-rust-app/workspace-1", "studios": [{"name": "Phone"}]}"#.to_vec();
        st.lock().unwrap().files.insert("doc".into(), theirs.clone());
        assert_eq!(check(&c, &link_for(&o.local).unwrap()).unwrap(), Some(sha256_hex(&theirs)));
        let mine2 = br#"{"format": "cad-rust-app/workspace-1", "studios": [{"name": "Mine 2"}]}"#.to_vec();
        std::fs::write(&o.local, &mine2).unwrap();
        assert_eq!(push(&c, &o.local, None).unwrap_err(), SaveError::Stale(Some(sha256_hex(&theirs))));
        assert_eq!(st.lock().unwrap().files["doc"], theirs);
        // opening it again keeps the unsent save here
        assert!(open(&c, &docs[0]).unwrap().kept_local);
        assert_eq!(std::fs::read(&o.local).unwrap(), mine2);

        // keep mine: replaces theirs (only the copy just seen)
        push(&c, &o.local, Some(&sha256_hex(&theirs))).unwrap();
        assert_eq!(st.lock().unwrap().files["doc"], mine2);
        // reload theirs
        st.lock().unwrap().files.insert("doc".into(), theirs.clone());
        reload(&c, &o.local).unwrap();
        assert_eq!(std::fs::read(&o.local).unwrap(), theirs);
        assert_eq!(check(&c, &link_for(&o.local).unwrap()).unwrap(), None);

        // a chat changed here goes back with the document
        let mut m: crate::claude_chats::Meta = serde_json::from_slice(&chat).unwrap();
        m.lines.push(crate::claude_chats::Line::User { text: "from the PC".into() });
        crate::claude_chats::save(&o.local, &mut m, &Default::default()).unwrap();
        let p = push(&c, &o.local, None).unwrap();
        assert_eq!(p.chats, 2); // c1.json changed, c1.context.json is new
        assert!(String::from_utf8_lossy(&st.lock().unwrap().files[".rcad-chats/c1.json"]).contains("from the PC"));
        assert_eq!(push(&c, &o.local, None).unwrap().chats, 0); // nothing new the second time

        // save as a copy: a new document, which the working copy becomes
        std::fs::write(&o.local, &mine).unwrap();
        let n = push_copy(&c, &o.local, "bracket (PC)").unwrap();
        assert_eq!(st.lock().unwrap().files["copy"], mine);
        assert_eq!(st.lock().unwrap().puts.last().unwrap(), &("copy".to_string(), "bracket%20%28PC%29".to_string()));
        assert_eq!(n, tmp.join("netdash").join("0").join("bracket").join("copy.json"));
        assert_eq!(link_for(&n).unwrap().id, "0-YnJhY2tldC9jb3B5Lmpzb24");
        assert!(!o.local.exists() && link_for(&o.local).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
