//! Project folders: a project is a folder named after its document, holding the document
//! (`<name>/<name>.json`) and anything that goes with it (exports, G-code, notes), usually
//! tracked with git (see `vcs.rs`). Projects are exported as a ZIP of that folder, git history
//! included, and imported from one into the projects folder.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::workspace::Workspace;

/// Where new and imported projects go: `RCAD_PROJECTS`, else "rCAD Projects" in Documents
/// (or in the home folder).
pub fn default_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("RCAD_PROJECTS") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let docs = home.join("Documents");
    if docs.is_dir() { docs.join("rCAD Projects") } else { home.join("rCAD Projects") }
}

/// A file or folder name from what the user typed (no separators or reserved characters).
pub fn clean_name(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '-' } else { c }).collect();
    let s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if s.is_empty() { "Project".into() } else { s }
}

/// The folder typed for a new project: `~` is the home folder, and it must be a full path.
pub fn expand_dir(typed: &str) -> Result<PathBuf, String> {
    let t = typed.trim();
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let p = match t.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => home().ok_or("no home folder")?.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(t),
    };
    if p.is_absolute() { Ok(p) } else { Err(format!("enter the folder's full path, like {}", default_dir().display())) }
}

/// `parent/name`, or `parent/name 2` and so on when that exists.
fn free_dir(parent: &Path, name: &str) -> PathBuf {
    let mut dir = parent.join(name);
    let mut n = 2;
    while dir.exists() {
        dir = parent.join(format!("{name} {n}"));
        n += 1;
    }
    dir
}

/// Make a project folder in `parent` and return the path its document is to be saved at.
pub fn create(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = clean_name(name);
    let dir = free_dir(parent, &name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let file = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name);
    Ok(dir.join(format!("{file}.json")))
}

/// Whether the document's folder is named after it (a project folder).
pub fn is_project_folder(doc: &Path) -> bool {
    doc.parent().and_then(|d| d.file_name()).is_some_and(|n| Some(n) == doc.file_stem())
}

/// The project folder of a document: its folder when that is named after it (a project folder),
/// else its git repository (unless that is the home folder or above, kept in git for settings);
/// None for a loose document in a shared folder.
pub fn folder_of(doc: &Path) -> Option<PathBuf> {
    if is_project_folder(doc) {
        return doc.parent().map(Path::to_path_buf);
    }
    let (root, _) = crate::vcs::locate(doc)?;
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).and_then(|h| std::fs::canonicalize(h).ok());
    (!home.is_some_and(|h| h.starts_with(&root))).then_some(root)
}

/// Whether an archive path (inside the project) is git's own data that could run commands or
/// point git elsewhere: a repository's config, its hooks, a `.git` file pointing to another
/// folder. (`git~1` is the short name Windows can give `.git`.)
fn unsafe_git_path(p: &Path, is_dir: bool) -> bool {
    // as Windows reads names: no case, trailing dots and spaces dropped, ":stream" ignored
    let names: Vec<String> = p.components().map(|c| c.as_os_str().to_string_lossy().split(':').next().unwrap_or_default().trim_end_matches(['.', ' ']).to_ascii_lowercase()).collect();
    let Some(i) = names.iter().position(|n| n == ".git" || n == "git~1") else { return false };
    let rest = &names[i + 1..];
    if rest.is_empty() {
        // `.git` as a file points git at another folder
        return !is_dir;
    }
    // (a submodule's or worktree's own repository, under modules/ or worktrees/, has them too;
    // elsewhere, under refs/ say, those are branch names)
    let nested = matches!(rest[0].as_str(), "modules" | "worktrees");
    let special = matches!(rest.last().map(String::as_str), Some("config" | "config.worktree" | "commondir" | "gitdir"));
    rest[0] == "hooks" || (nested && rest.iter().any(|n| n == "hooks")) || (special && (rest.len() == 1 || nested))
}

// ------------------------------------------------------------ export

fn crc32(b: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(b);
    h.finalize()
}

/// A ZIP archive of `entries` (path, contents; a path ending in '/' is a folder), deflated.
pub fn zip(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    let mut cd = vec![];
    for (name, data) in entries {
        let (method, body): (u16, Vec<u8>) = if name.ends_with('/') || data.is_empty() {
            (0, vec![])
        } else {
            let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(data).map_err(|e| e.to_string())?;
            let d = z.finish().map_err(|e| e.to_string())?;
            if d.len() < data.len() { (8, d) } else { (0, data.clone()) }
        };
        if out.len() > u32::MAX as usize - body.len() - 1024 || data.len() > u32::MAX as usize {
            return Err("the project is over 4 GB, too big to export".into());
        }
        let (crc, off) = (crc32(data), out.len() as u32);
        let n = name.as_bytes();
        // local header: version 2.0, flags (bit 11: UTF-8 names), method, no time, crc, sizes, name
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 8]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]); // 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(n.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(n);
        out.extend_from_slice(&body);
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&[20, 3, 20, 0, 0, 8]); // made by Unix, version 2.0, UTF-8
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&[0, 0, 0x21, 0]);
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&(body.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(n.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0; 8]);
        // external attributes: rw-r--r-- files, rwxr-xr-x folders
        let attr: u32 = if name.ends_with('/') { (0o40755 << 16) | 0x10 } else { 0o100644 << 16 };
        cd.extend_from_slice(&attr.to_le_bytes());
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(n);
    }
    if entries.len() > u16::MAX as usize {
        return Err("the project has too many files to export".into());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    Ok(out)
}

/// Every file and folder under `dir`, as archive paths under `prefix`.
fn collect(dir: &Path, prefix: &str, history: bool, skip: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let mut items: Vec<_> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name().to_string_lossy().into_owned();
        let t = e.file_type().map_err(|e| e.to_string())?;
        if (!history && name == ".git") || t.is_symlink() || e.path() == skip {
            continue;
        }
        let path = format!("{prefix}{name}");
        if t.is_dir() {
            out.push((format!("{path}/"), vec![]));
            collect(&e.path(), &format!("{path}/"), history, skip, out)?;
        } else {
            let mut data = vec![];
            std::fs::File::open(e.path()).and_then(|mut f| f.read_to_end(&mut data)).map_err(|err| format!("could not read {}: {err}", e.path().display()))?;
            out.push((path, data));
        }
    }
    Ok(())
}

/// Write the document's project (its whole folder, git history too when `history`) to a ZIP at
/// `to`. A loose document is exported on its own, in a folder named after it.
/// Returns how many files went in.
pub fn export(doc: &Path, to: &Path, history: bool) -> Result<usize, String> {
    let stem = doc.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    let mut entries = vec![];
    match folder_of(doc) {
        Some(dir) => {
            let top = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(stem);
            entries.push((format!("{top}/"), vec![]));
            // the archive itself may be saved in the project folder: leave out an older copy of it
            let skip = to.parent().and_then(|p| std::fs::canonicalize(p).ok()).map(|p| p.join(to.file_name().unwrap_or_default())).unwrap_or_default();
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            collect(&dir, &format!("{top}/"), history, &skip, &mut entries)?;
        }
        None => {
            let data = std::fs::read(doc).map_err(|e| e.to_string())?;
            let file = doc.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            entries.push((format!("{stem}/"), vec![]));
            entries.push((format!("{stem}/{file}"), data));
        }
    }
    let files = entries.iter().filter(|(p, _)| !p.ends_with('/')).count();
    let bytes = zip(&entries)?;
    std::fs::write(to, bytes).map_err(|e| format!("could not write {}: {e}", to.display()))?;
    Ok(files)
}

// ------------------------------------------------------------ import

/// A safe relative path from an archive entry (no "..", no absolute paths).
fn safe(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Unpack a project ZIP into a new folder in `parent` and return the path of its document.
pub fn import(zip_path: &Path, parent: &Path) -> Result<PathBuf, String> {
    let bytes = std::fs::read(zip_path).map_err(|e| format!("could not read {}: {e}", zip_path.display()))?;
    let entries = crate::grabcad::unzip(&bytes)?;
    let mut files: Vec<(PathBuf, bool, Vec<u8>)> =
        entries.into_iter().filter(|(n, _)| !n.starts_with("__MACOSX/")).filter_map(|(n, d)| safe(&n).map(|p| (p, n.ends_with('/'), d))).collect();
    if files.is_empty() {
        return Err("the ZIP is empty".into());
    }
    // one top folder in the archive: that is the project folder
    let top = files[0].0.components().next().map(|c| c.as_os_str().to_owned());
    let single_top = top.is_some() && files.iter().all(|(p, dir, _)| p.components().next().map(|c| c.as_os_str()) == top.as_deref() && (*dir || p.components().count() > 1));
    let zip_stem = zip_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    let name = if single_top { top.as_ref().map(|t| t.to_string_lossy().into_owned()).unwrap_or(zip_stem) } else { zip_stem };
    if single_top {
        for f in &mut files {
            f.0 = f.0.components().skip(1).collect();
        }
    }
    // the document: a workspace or part at the top, named after the project if there are several
    let docs: Vec<&PathBuf> = files
        .iter()
        .filter(|(p, dir, d)| {
            !dir && p.components().count() == 1
                && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"))
                && std::str::from_utf8(d).ok().is_some_and(|s| Workspace::from_json(s).is_ok())
        })
        .map(|(p, _, _)| p)
        .collect();
    let doc = docs
        .iter()
        .find(|p| p.file_stem().map(|s| s.to_string_lossy()) == Some(name.as_str().into()))
        .or(docs.first())
        .map(|p| (*p).clone())
        .ok_or("the ZIP has no rCAD document at its top level")?;
    let dir = free_dir(parent, &clean_name(&name));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    // the repository's own config is rebuilt below from its safe settings
    let git_config = files.iter().find(|(p, d, _)| !d && *p == Path::new(".git").join("config")).map(|f| f.2.clone());
    for (p, is_dir, data) in &files {
        if p.as_os_str().is_empty() || unsafe_git_path(p, *is_dir) {
            continue;
        }
        let out = dir.join(p);
        if *is_dir {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(d) = out.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&out, data).map_err(|e| format!("could not write {}: {e}", out.display()))?;
        }
    }
    // git keeps these folders even when empty, and won't see a repository without them
    if dir.join(".git").is_dir() {
        for d in ["refs/heads", "refs/tags", "objects"] {
            let _ = std::fs::create_dir_all(dir.join(".git").join(d));
        }
        crate::vcs::write_imported_config(&dir.join(".git"), git_config.as_deref())?;
    }
    Ok(dir.join(doc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_and_import_round_trip_with_history() {
        let base = std::env::temp_dir().join(format!("rcad-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let doc = create(&base, "Robot: arm").unwrap();
        assert_eq!(doc, base.join("Robot- arm").join("Robot- arm.json"));
        std::fs::write(&doc, Workspace::default().to_json()).unwrap();
        std::fs::create_dir_all(doc.parent().unwrap().join("cam")).unwrap();
        std::fs::write(doc.parent().unwrap().join("cam/op1.tap"), "G0 X0\n".repeat(100)).unwrap();
        assert_eq!(folder_of(&doc).as_deref(), doc.parent());
        let git = crate::vcs::version().is_some();
        if git {
            crate::vcs::init(&doc, &crate::vcs::Identity { name: "T".into(), email: "t@x".into() }).unwrap();
        }
        let z = base.join("out.zip");
        let n = export(&doc, &z, true).unwrap();
        assert!(n >= 2);
        let into = base.join("imported");
        let got = import(&z, &into).unwrap();
        assert_eq!(got, into.join("Robot- arm").join("Robot- arm.json"));
        assert_eq!(std::fs::read_to_string(into.join("Robot- arm/cam/op1.tap")).unwrap(), "G0 X0\n".repeat(100));
        if git {
            let s = crate::vcs::status(&got).unwrap();
            assert!(!s.changed);
            assert_eq!(crate::vcs::history(&s.root, &s.rel, 10).len(), 1);
        }
        // a second import doesn't overwrite the first
        assert_eq!(import(&z, &into).unwrap(), into.join("Robot- arm 2").join("Robot- arm.json"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn imported_repository_keeps_no_commands() {
        let base = std::env::temp_dir().join(format!("rcad-project-evil-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let evil = b"[core]\n\trepositoryformatversion = 0\n\tfsmonitor = touch PWNED\n".to_vec();
        let entries = vec![
            ("Arm/".to_string(), vec![]),
            ("Arm/Arm.json".to_string(), Workspace::default().to_json().into_bytes()),
            ("Arm/.git/HEAD".to_string(), b"ref: refs/heads/main\n".to_vec()),
            ("Arm/.git/config".to_string(), evil.clone()),
            ("Arm/.git/hooks/post-commit".to_string(), b"#!/bin/sh\ntouch PWNED\n".to_vec()),
            ("Arm/.git/refs/heads/config".to_string(), b"0000000000000000000000000000000000000000\n".to_vec()),
            ("Arm/sub/.git".to_string(), b"gitdir: ../evil\n".to_vec()),
            ("Arm/evil/config".to_string(), evil),
        ];
        let z = base.join("in.zip");
        std::fs::write(&z, zip(&entries).unwrap()).unwrap();
        let got = import(&z, &base.join("out")).unwrap();
        let dir = got.parent().unwrap();
        assert!(!dir.join(".git/hooks/post-commit").exists());
        assert!(!dir.join("sub/.git").exists());
        // a branch named "config" is a branch, and a project file named config is a project file
        assert!(dir.join(".git/refs/heads/config").exists() && dir.join("evil/config").exists());
        if crate::vcs::version().is_some() {
            let cfg = std::fs::read_to_string(dir.join(".git/config")).unwrap();
            assert!(!cfg.contains("PWNED"), "{cfg}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn git_internals_that_run_commands() {
        let u = |p: &str, d: bool| unsafe_git_path(Path::new(p), d);
        assert!(u(".git/config", false) && u(".GIT./Config", false) && u("git~1/config", false));
        assert!(u(".git/hooks/pre-commit", false) && u(".git/commondir", false) && u("a/.git", false));
        assert!(u(".git/modules/lib/config", false) && u(".git/modules/lib/hooks/x", false));
        assert!(!u(".git", true) && !u(".git/HEAD", false) && !u(".git/refs/heads/config", false) && !u("cam/config", false));
    }

    #[test]
    fn new_project_folders() {
        let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap());
        assert_eq!(expand_dir("~/CAD").unwrap(), home.join("CAD"));
        assert_eq!(expand_dir(" ~ ").unwrap(), home);
        assert!(expand_dir("CAD").is_err());
    }

    #[test]
    fn unsafe_paths_are_dropped() {
        assert!(safe("../x").is_none());
        assert!(safe("/etc/passwd").is_none());
        assert_eq!(safe("a/./b").unwrap(), PathBuf::from("a/b"));
    }
}
