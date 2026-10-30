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

/// The project folder of a document: its git repository, or its folder when that is named after
/// it (a project folder); None for a loose document in a shared folder.
pub fn folder_of(doc: &Path) -> Option<PathBuf> {
    if let Some((root, _)) = crate::vcs::locate(doc) {
        return Some(root);
    }
    let dir = doc.parent()?;
    (dir.file_name().is_some() && dir.file_name() == doc.file_stem()).then(|| dir.to_path_buf())
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
    for (p, is_dir, data) in &files {
        if p.as_os_str().is_empty() {
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
    fn unsafe_paths_are_dropped() {
        assert!(safe("../x").is_none());
        assert!(safe("/etc/passwd").is_none());
        assert_eq!(safe("a/./b").unwrap(), PathBuf::from("a/b"));
    }
}
