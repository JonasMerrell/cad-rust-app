//! Built geometry kept between sessions, so opening a document (or a project someone exported)
//! doesn't regenerate every Part Studio.
//!
//! A Part Studio's rebuild result (meshes, edges, frames, feature status) is stored under a key
//! that hashes the app version and the studio's whole document (and every studio's, when it
//! derives parts from another). Any change to the document gives another key, so a stale entry
//! is never used: only the studios that changed are rebuilt.
//!
//! Entries live in the project folder (`<project>/.rcad-cache/`), so a project ZIP carries them,
//! or in the user's cache folder for documents that aren't in a project folder. They are written
//! when the document is saved, and as studios finish building while the document has no unsaved
//! changes (so a document opened once opens quickly the next time). A project's folder keeps only
//! the entries of the document as saved; the user cache is trimmed to [`USER_LIMIT`].
//!
//! `RCAD_NO_BUILD_CACHE=1` turns it off; `RCAD_BUILD_CACHE` moves the user cache.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::doc::{Document, Op};
use crate::kernel::RebuildResult;

/// Bump when the kernel's output changes in a way the app version doesn't capture.
const FORMAT: u32 = 1;
/// The cache folder inside a project folder.
pub const DIR: &str = ".rcad-cache";
const EXT: &str = "json.gz";
/// Most the user cache may hold before the oldest entries go.
const USER_LIMIT: u64 = 2 << 30;

/// Entries still being written in the background.
static WRITING: AtomicUsize = AtomicUsize::new(0);

pub fn enabled() -> bool {
    std::env::var_os("RCAD_NO_BUILD_CACHE").is_none_or(|v| v.is_empty() || v == "0")
}

/// Whether the studio brings in parts from other studios (its result then depends on them too).
pub fn derives(doc: &Document) -> bool {
    doc.features.iter().any(|f| matches!(f.op, Op::Derived { .. }))
}

/// FNV-1a, 64 bit: stable across builds and platforms (unlike `DefaultHasher`).
struct Fnv(u64);

impl Write for Fnv {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for b in buf {
            self.0 ^= *b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The key of a studio's rebuild result. `studios` gives the JSON of every Part Studio (as
/// sent to the kernel), asked for only when `doc` derives parts from them. Never 0.
pub fn key(doc: &Document, studios: impl FnOnce() -> String) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    let _ = write!(h, "rcad {} build {FORMAT}\n", env!("CARGO_PKG_VERSION"));
    let _ = serde_json::to_writer(&mut h, doc);
    if derives(doc) {
        let _ = h.write_all(b"\nstudios\n");
        let _ = h.write_all(studios().as_bytes());
    }
    h.0.max(1)
}

fn file_name(key: u64) -> String {
    format!("{key:016x}.{EXT}")
}

fn key_of(name: &str) -> Option<u64> {
    let hex = name.strip_suffix(&format!(".{EXT}"))?;
    (hex.len() == 16).then(|| u64::from_str_radix(hex, 16).ok()).flatten()
}

/// The user's cache folder for documents outside project folders.
pub fn user_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RCAD_BUILD_CACHE").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| home().map(|h| h.join(".cache")))?;
    Some(base.join("cad-rust-app").join("builds"))
}

/// Where to look for a document's entries, the folder written to first. A document in a
/// project folder (or next to a cache folder) uses that cache, and the user cache as well, for
/// entries made before it was moved there; any other uses the user cache.
pub fn dirs_for(doc: Option<&Path>) -> Vec<PathBuf> {
    if !enabled() {
        return vec![];
    }
    let mut dirs = vec![];
    if let Some(doc) = doc {
        // a project folder, or a folder that has a cache already (a project imported next to
        // another of the same name lands in "<name> 2" and keeps its document's name)
        let dir = if crate::project::is_project_folder(doc) { crate::project::folder_of(doc) } else { doc.parent().filter(|d| d.join(DIR).is_dir()).map(Path::to_path_buf) };
        dirs.extend(dir.map(|d| d.join(DIR)));
    }
    dirs.extend(user_dir());
    dirs
}

/// The entry for `key`, if one of `dirs` has it.
pub fn find(dirs: &[PathBuf], key: u64) -> Option<PathBuf> {
    let name = file_name(key);
    dirs.iter().map(|d| d.join(&name)).find(|p| p.is_file())
}

/// Read an entry (None when it is damaged or from an incompatible build).
pub fn load(path: &Path, key: u64) -> Option<RebuildResult> {
    let file = std::fs::File::open(path).ok()?;
    let mut json = vec![];
    flate2::read::GzDecoder::new(std::io::BufReader::new(file)).read_to_end(&mut json).ok()?;
    let mut r: RebuildResult = serde_json::from_slice(&json).ok()?;
    r.key = key;
    Some(r)
}

fn encode(r: &RebuildResult) -> std::io::Result<Vec<u8>> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    serde_json::to_writer(&mut gz, r)?;
    gz.finish()
}

/// Write `r` under its key into `dir` (now; see [`store`] for the background version).
pub fn store_now(dir: &Path, r: &RebuildResult) -> std::io::Result<()> {
    if r.key == 0 {
        return Ok(());
    }
    let path = dir.join(file_name(r.key));
    if path.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        // not for version control: it is remade from the document
        let _ = std::fs::write(&ignore, "# rCAD's built geometry, remade from the document when missing\n*\n");
    }
    let bytes = encode(r)?;
    // a name of its own, so two writers of the same entry don't trip over each other
    static N: AtomicUsize = AtomicUsize::new(0);
    let tmp = dir.join(format!(".{:016x}.{}.{}.tmp", r.key, std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let res = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Write `r` under its key into `dir` on a background thread (unless it is there already).
pub fn store(dir: &Path, r: &RebuildResult) {
    if r.key == 0 || dir.join(file_name(r.key)).is_file() {
        return;
    }
    let (dir, r) = (dir.to_path_buf(), r.clone());
    WRITING.fetch_add(1, Ordering::SeqCst);
    std::thread::spawn(move || {
        if let Err(e) = store_now(&dir, &r) {
            log::warn!("could not cache built geometry in {}: {e}", dir.display());
        }
        WRITING.fetch_sub(1, Ordering::SeqCst);
    });
}

/// Wait (up to `limit`) for background writes to finish, e.g. before a project is exported.
pub fn wait_written(limit: std::time::Duration) {
    let start = std::time::Instant::now();
    while WRITING.load(Ordering::SeqCst) > 0 && start.elapsed() < limit {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Entries in `dir` (key, path, size, modified).
fn entries(dir: &Path) -> Vec<(u64, PathBuf, u64, std::time::SystemTime)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| {
            let k = key_of(&e.file_name().to_string_lossy())?;
            let m = e.metadata().ok()?;
            Some((k, e.path(), m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect()
}

/// Remove a project's entries other than `keep` (the studios of the document as saved).
pub fn prune_project(dir: &Path, keep: &HashSet<u64>) {
    for (k, p, _, _) in entries(dir) {
        if !keep.contains(&k) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Trim the user cache to `limit` bytes, oldest entries first.
pub fn prune_user(dir: &Path, limit: u64) {
    let mut all = entries(dir);
    let mut total: u64 = all.iter().map(|e| e.2).sum();
    if total <= limit {
        return;
    }
    all.sort_by_key(|e| e.3);
    for (_, p, size, _) in all {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(p).is_ok() {
            total -= size;
        }
    }
}

/// The keys of every studio in `docs` (the workspace's Part Studios, as sent to the kernel).
pub fn keys(docs: &[Document]) -> Vec<u64> {
    let mut json: Option<String> = None;
    docs.iter().map(|d| key(d, || json.get_or_insert_with(|| serde_json::to_string(docs).unwrap_or_default()).clone())).collect()
}

/// After a save to `doc_path`: store every studio result that matches the document as saved,
/// then drop entries the project no longer needs.
pub fn saved(app: &crate::app::App, doc_path: &Path) {
    let dirs = dirs_for(Some(doc_path));
    let Some(dir) = dirs.first() else { return };
    let docs = app.studio_docs();
    let keys = keys(&docs);
    for (i, k) in keys.iter().enumerate() {
        let r = if i == app.cur_studio { Some(&app.result) } else { app.studio_results.get(&i) };
        if let Some(r) = r.filter(|r| r.key == *k) {
            store(dir, r);
        }
    }
    if dirs.len() > 1 {
        // a project folder: it keeps just what opening the saved document needs
        let keep: HashSet<u64> = keys.into_iter().collect();
        prune_project(dir, &keep);
    } else {
        prune_user(dir, USER_LIMIT);
    }
}

/// Drop the entries for `keys` from `dirs` (so the next rebuild really rebuilds).
pub fn forget(dirs: &[PathBuf], keys: &[u64]) {
    for d in dirs {
        for k in keys {
            let _ = std::fs::remove_file(d.join(file_name(*k)));
        }
    }
}

/// Entries a ZIP of a loose document (one outside a project folder) should carry, as
/// (file name, bytes): those of its studios found in the user cache.
pub fn entries_for(doc_path: &Path) -> Vec<(String, Vec<u8>)> {
    let Some(user) = user_dir().filter(|_| enabled()) else { return vec![] };
    let Ok(ws) = std::fs::read_to_string(doc_path).map_err(|e| e.to_string()).and_then(|s| crate::workspace::Workspace::from_json(&s)) else {
        return vec![];
    };
    let docs: Vec<Document> = ws.studios.into_iter().map(|s| s.doc).collect();
    let mut seen = HashSet::new();
    keys(&docs)
        .into_iter()
        .filter(|k| seen.insert(*k))
        .filter_map(|k| {
            let name = file_name(k);
            std::fs::read(user.join(&name)).ok().map(|b| (name, b))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(len: &str) -> Document {
        let mut d = Document::default();
        d.variables.push(crate::doc::Variable { name: "len".into(), expr: len.into() });
        d
    }

    #[test]
    fn keys_follow_the_document() {
        let a = studio("10");
        let k = key(&a, String::new);
        assert_eq!(k, key(&a.clone(), String::new), "same document, same key");
        assert_ne!(k, key(&studio("11"), String::new), "an edit gives a new key");
        // stable through a save and reopen
        let ws = crate::workspace::Workspace { studios: vec![crate::workspace::Studio { name: "A".into(), doc: a }], ..Default::default() };
        let back = crate::workspace::Workspace::from_json(&ws.to_json()).unwrap();
        assert_eq!(key(&back.studios[0].doc, String::new), k);
        // other studios only matter to a studio deriving from them
        assert_eq!(keys(&[studio("10"), studio("1")])[0], keys(&[studio("10"), studio("2")])[0]);
        let mut d = studio("10");
        d.features.push(crate::doc::Feature { id: 1, name: "Derived 1".into(), suppressed: false, hidden: false, op: Op::Derived { studio: 1, bodies: vec![] } });
        assert_ne!(keys(&[d.clone(), studio("1")])[0], keys(&[d, studio("2")])[0]);
    }

    #[test]
    fn store_find_load_prune() {
        let dir = std::env::temp_dir().join(format!("rcad-build-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = RebuildResult::default();
        r.bodies.push(crate::kernel::BodyMesh { name: "Part 1".into(), positions: vec![0.0, 1.5, -2.25], volume: 12.5, ..Default::default() });
        r.key = 0x1234;
        store_now(&dir, &r).unwrap();
        let p = find(&[dir.join("nope"), dir.clone()], 0x1234).expect("stored entry is found");
        let back = load(&p, 0x1234).unwrap();
        assert_eq!(back.key, 0x1234);
        assert_eq!(back.bodies[0].positions, vec![0.0, 1.5, -2.25]);
        assert_eq!(back.bodies[0].volume, 12.5);
        assert!(find(&[dir.clone()], 0x9999).is_none());
        // a damaged entry is a miss, not an error
        std::fs::write(dir.join(file_name(0x5555)), b"not gzip").unwrap();
        assert!(load(&dir.join(file_name(0x5555)), 0x5555).is_none());
        prune_project(&dir, &[0x1234].into_iter().collect());
        assert!(find(&[dir.clone()], 0x5555).is_none());
        assert!(find(&[dir.clone()], 0x1234).is_some());
        prune_user(&dir, 0);
        assert!(find(&[dir.clone()], 0x1234).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
