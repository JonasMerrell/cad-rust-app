//! The KiCad community libraries: the official KiCad symbol, footprint and 3D model libraries
//! (gitlab.com/kicad/libraries), searched and placed from the PCB tab.
//!
//! The libraries are licensed CC-BY-SA 4.0 with an exception for designs made with them, so
//! rCAD does not ship them: it uses a KiCad installation's copy when there is one, and
//! otherwise downloads a tagged release into the user's cache on first use. 3D models are
//! fetched one at a time when a board is turned into 3D.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::pcb_kicad::{self, Sx};
use crate::pcb_lib::{self, Footprint, LibSymbol};

/// The KiCad library release downloaded when KiCad itself isn't installed.
pub const VERSION: &str = "10.0.8";
const API: &str = "https://gitlab.com/api/v4/projects/kicad%2Flibraries%2F";
const RAW: &str = "https://gitlab.com/kicad/libraries/";

/// The credit and licence shown wherever the libraries are used.
pub const CREDIT: &str = "KiCad libraries © the KiCad Libraries Team and contributors, CC-BY-SA 4.0";
pub const LICENSE: &str = "The KiCad libraries are licensed under CC-BY-SA 4.0, with an exception: \
designs that use them, and the files made from those designs (Gerbers, STEP, BOMs, …), are not bound by \
the licence's share-alike terms. Sharing the library files themselves, or changed copies of them, keeps \
the CC-BY-SA licence and its credit.";
pub const LICENSE_URL: &str = "https://www.kicad.org/libraries/license/";

/// Digi-Key's KiCad library: about 1,000 orderable parts, each symbol with its footprint and
/// Digi-Key and manufacturer part numbers (KiCad 5 format, no longer updated by Digi-Key).
pub const DK_ZIP: &str = "https://codeload.github.com/Digi-Key/digikey-kicad-library/zip/refs/heads/master";
pub const DK_HOME: &str = "https://github.com/Digi-Key/digikey-kicad-library";
pub const DK_CREDIT: &str = "Digi-Key KiCad Library © Digi-Key Corporation, CC-BY-SA 4.0";
pub const DK_LICENSE: &str = "Digi-Key's KiCad library is licensed under CC-BY-SA 4.0, with an exception: designs \
     made with it, and the files generated from them, are not covered by the licence.";
/// The footprint library name Digi-Key's symbols refer to.
pub const DK_FOOTPRINTS: &str = "digikey-footprints";
/// Marks index entries that come from Digi-Key's library (`SymEntry::file`).
const DK_FILE: &str = "digikey:";

/// Where a source's symbols, footprints and 3D models are.
#[derive(Clone, Debug, PartialEq)]
pub struct Root {
    /// Shown in the browser ("KiCad 10.0.8 (downloaded)", "KiCad installed in /usr/share/kicad").
    pub label: String,
    pub symbols: PathBuf,
    pub footprints: PathBuf,
    pub models: Option<PathBuf>,
}

/// One symbol in the index.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SymEntry {
    pub lib: String,
    pub name: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub keys: String,
    /// Default footprint ("Lib:Name").
    #[serde(default)]
    pub fp: String,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default)]
    pub pins: u32,
    #[serde(default)]
    pub power: bool,
    #[serde(default)]
    pub datasheet: String,
    /// The file it is in, relative to the symbol root.
    pub file: String,
    /// The symbol it takes its drawing from, if any.
    #[serde(default)]
    pub extends: String,
    /// Order numbers, for libraries that carry them (Digi-Key's).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digikey: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mpn: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manufacturer: String,
}

impl SymEntry {
    pub fn key(&self) -> String {
        format!("{}:{}", self.lib, self.name)
    }
    /// From Digi-Key's KiCad library.
    pub fn from_digikey(&self) -> bool {
        self.file.starts_with(DK_FILE)
    }
}

/// One footprint in the index.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FpEntry {
    pub lib: String,
    pub name: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub tags: String,
    /// Distinct numbered pads.
    #[serde(default)]
    pub pads: u32,
    #[serde(default)]
    pub smd: bool,
    #[serde(default)]
    pub model: String,
}

impl FpEntry {
    pub fn key(&self) -> String {
        format!("{}:{}", self.lib, self.name)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Index {
    /// The root the index was made from (its label), to rebuild when it changes.
    pub label: String,
    pub symbols: Vec<SymEntry>,
    pub footprints: Vec<FpEntry>,
}

// ---------------------------------------------------------------- locations

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// rCAD's cache folder for the libraries.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("RCAD_KICAD_CACHE") {
        return Some(PathBuf::from(d));
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| home().map(|h| h.join(".cache")))?;
    Some(base.join("cad-rust-app").join("kicad"))
}

/// The downloaded release's folder.
fn download_dir() -> Option<PathBuf> {
    cache_dir().map(|d| d.join(VERSION))
}

/// A folder laid out like KiCad's `share/kicad` (symbols/, footprints/, 3dmodels/).
fn share_root(dir: &Path, label: String) -> Option<Root> {
    let symbols = dir.join("symbols");
    let footprints = dir.join("footprints");
    if !symbols.is_dir() || !footprints.is_dir() {
        return None;
    }
    let models = Some(dir.join("3dmodels")).filter(|m| m.is_dir());
    Some(Root { label, symbols, footprints, models })
}

/// A KiCad installation's libraries: `KICADn_SYMBOL_DIR`/`KICADn_FOOTPRINT_DIR`, else the usual
/// install folders on Linux, macOS and Windows.
fn installed() -> Option<Root> {
    for v in ["10", "9", "8"] {
        let s = std::env::var_os(format!("KICAD{v}_SYMBOL_DIR")).map(PathBuf::from);
        let f = std::env::var_os(format!("KICAD{v}_FOOTPRINT_DIR")).map(PathBuf::from);
        if let (Some(s), Some(f)) = (s, f) {
            if s.is_dir() && f.is_dir() {
                let models = std::env::var_os(format!("KICAD{v}_3DMODEL_DIR")).map(PathBuf::from).filter(|m| m.is_dir());
                return Some(Root { label: format!("KiCad {v} libraries in {}", s.parent().unwrap_or(&s).display()), symbols: s, footprints: f, models });
            }
        }
    }
    let mut dirs: Vec<PathBuf> = vec!["/usr/share/kicad".into(), "/usr/local/share/kicad".into(), "/Applications/KiCad/KiCad.app/Contents/SharedSupport".into()];
    if let Some(h) = home() {
        dirs.push(h.join(".var/app/org.kicad.KiCad/data/kicad"));
    }
    for pf in [std::env::var_os("ProgramFiles"), Some("C:\\Program Files".into())].into_iter().flatten() {
        for v in ["10.0", "9.0", "8.0"] {
            dirs.push(PathBuf::from(&pf).join("KiCad").join(v).join("share").join("kicad"));
        }
    }
    dirs.into_iter().find_map(|d| {
        let label = format!("KiCad libraries installed in {}", d.display());
        share_root(&d, label)
    })
}

/// The downloaded release, if it is complete.
fn downloaded() -> Option<Root> {
    let d = download_dir()?;
    if !d.join("complete").exists() {
        return None;
    }
    share_root(&d, format!("KiCad {VERSION} libraries (downloaded)"))
}

/// Digi-Key's library: `RCAD_DIGIKEY_LIB_DIR` (a checkout of the repository), else the download
/// in the cache once it is complete.
pub fn digikey_dir() -> Option<PathBuf> {
    let ok = |d: &Path| d.join("digikey-symbols").is_dir() && d.join(format!("{DK_FOOTPRINTS}.pretty")).is_dir();
    if let Some(d) = std::env::var_os("RCAD_DIGIKEY_LIB_DIR").map(PathBuf::from) {
        if ok(&d) {
            return Some(d);
        }
    }
    let d = cache_dir()?.join("digikey-kicad-library");
    (d.join("complete").exists() && ok(&d)).then_some(d)
}

/// The index's label: the KiCad root's, and whether Digi-Key's library is in it.
fn ix_label(root: &Root) -> String {
    match digikey_dir() {
        Some(_) => format!("{} + Digi-Key library", root.label),
        None => root.label.clone(),
    }
}

/// Where the libraries are: `RCAD_KICAD_DIR` (a share/kicad-like folder), the downloaded release
/// (once there is one), else an installed KiCad.
pub fn root() -> Option<Root> {
    if let Some(d) = std::env::var_os("RCAD_KICAD_DIR") {
        let d = PathBuf::from(d);
        if let Some(r) = share_root(&d, format!("KiCad libraries in {}", d.display())) {
            return Some(r);
        }
    }
    downloaded().or_else(installed)
}

// ---------------------------------------------------------------- downloading

/// Progress of a download or index build, shared with the UI thread.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub stage: String,
    /// 0..1
    pub done: f32,
    pub error: Option<String>,
    pub finished: bool,
}

pub type Shared = Arc<Mutex<Progress>>;

fn set(p: &Shared, stage: &str, done: f32) {
    if let Ok(mut g) = p.lock() {
        g.stage = stage.to_string();
        g.done = done;
    }
}

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout(Duration::from_secs(600)).try_proxy_from_env(true).user_agent("rCAD (KiCad library browser)").build()
}

fn get(url: &str, limit: u64, on: &mut dyn FnMut(u64, Option<u64>)) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let r = http().get(url).call().map_err(|e| match e {
        ureq::Error::Status(c, _) => format!("{url}: HTTP {c}"),
        ureq::Error::Transport(t) => format!("couldn't reach gitlab.com: {t}"),
    })?;
    let total = r.header("content-length").and_then(|v| v.parse().ok());
    let mut rd = r.into_reader().take(limit);
    let mut out = vec![];
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = rd.read(&mut buf).map_err(|e| format!("download of {url} broke off: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        on(out.len() as u64, total);
    }
    Ok(out)
}

/// Download the official symbol and footprint libraries (about 60 MB) into the cache and index
/// them. Runs on a worker thread; `p` reports how it goes.
pub fn download(p: &Shared) -> Result<(), String> {
    let dir = download_dir().ok_or("no cache folder (HOME is not set)")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(dir.join("complete"));
    for (k, (repo, sub, ext)) in [("kicad-symbols", "symbols", ".kicad_sym"), ("kicad-footprints", "footprints", ".kicad_mod")].into_iter().enumerate() {
        let base = k as f32 * 0.45;
        let url = format!("{API}{repo}/repository/archive.zip?sha={VERSION}");
        let label = format!("Downloading the KiCad {sub}");
        set(p, &label, base);
        let zip = get(&url, 400 << 20, &mut |got, total| {
            let f = total.map(|t| got as f32 / t as f32).unwrap_or(((got as f32) / 32e6).min(0.95));
            set(p, &format!("{label} ({:.0} MB)", got as f64 / 1e6), base + 0.35 * f);
        })?;
        set(p, &format!("Unpacking the KiCad {sub}"), base + 0.36);
        let out = dir.join(sub);
        let _ = std::fs::remove_dir_all(&out);
        let mut budget = 2usize << 30;
        let mut err = None;
        let keep = |n: &str| n.ends_with(ext) || n.ends_with("/LICENSE.md");
        crate::grabcad::unzip_each(&zip, &keep, &mut budget, &mut |name, data| {
            let Some(data) = data else { return };
            // drop the archive's top folder ("kicad-symbols-10.0.8-<sha>/")
            let rel = name.split_once('/').map(|x| x.1).unwrap_or(&name);
            if rel.contains("..") || rel.starts_with('/') {
                return;
            }
            let path = if rel == "LICENSE.md" { dir.join(format!("LICENSE-{sub}.md")) } else { out.join(rel) };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, data) {
                err.get_or_insert(format!("{}: {e}", path.display()));
            }
        })?;
        if let Some(e) = err {
            return Err(e);
        }
    }
    std::fs::create_dir_all(dir.join("3dmodels")).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("complete"), VERSION).map_err(|e| e.to_string())?;
    let root = downloaded().ok_or("the download is incomplete")?;
    build_index(&root, p)?;
    Ok(())
}

/// Download Digi-Key's KiCad library (about 10 MB from GitHub) into the cache, then re-index.
pub fn download_digikey(p: &Shared) -> Result<(), String> {
    let dir = cache_dir().ok_or("no cache folder (HOME is not set)")?.join("digikey-kicad-library");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    set(p, "Downloading Digi-Key's KiCad library", 0.05);
    let zip = get(DK_ZIP, 200 << 20, &mut |got, total| {
        let f = total.map(|t| got as f32 / t as f32).unwrap_or(((got as f32) / 12e6).min(0.95));
        set(p, &format!("Downloading Digi-Key's KiCad library ({:.0} MB)", got as f64 / 1e6), 0.05 + 0.7 * f);
    })?;
    set(p, "Unpacking Digi-Key's KiCad library", 0.8);
    let mut budget = 1usize << 30;
    let mut err = None;
    let keep = |n: &str| {
        let rel = n.split_once('/').map(|x| x.1).unwrap_or(n);
        (rel.starts_with("digikey-symbols/") && (rel.ends_with(".lib") || rel.ends_with(".dcm"))) || (rel.starts_with("digikey-footprints.pretty/") && rel.ends_with(".kicad_mod")) || rel == "LICENSE.md"
    };
    crate::grabcad::unzip_each(&zip, &keep, &mut budget, &mut |name, data| {
        let Some(data) = data else { return };
        let rel = name.split_once('/').map(|x| x.1).unwrap_or(&name);
        if rel.contains("..") || rel.starts_with('/') {
            return;
        }
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, data) {
            err.get_or_insert(format!("{}: {e}", path.display()));
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    std::fs::write(dir.join("complete"), "master").map_err(|e| e.to_string())?;
    if digikey_dir().is_none() {
        return Err("the Digi-Key library download is incomplete".into());
    }
    if let Some(root) = root() {
        build_index(&root, p)?;
    }
    Ok(())
}

/// `download_digikey` on a worker thread.
pub fn start_digikey() -> Shared {
    let p: Shared = Arc::default();
    let q = p.clone();
    std::thread::spawn(move || {
        let r = download_digikey(&q);
        if let Ok(mut g) = q.lock() {
            g.finished = true;
            g.done = 1.0;
            if let Err(e) = r {
                g.error = Some(e);
            }
        }
    });
    p
}

/// Start `download` (or, with libraries already present, an index build) on a worker thread.
pub fn start(download_too: bool) -> Shared {
    let p: Shared = Arc::default();
    let q = p.clone();
    std::thread::spawn(move || {
        let r = if download_too {
            download(&q)
        } else {
            match root() {
                Some(r) => build_index(&r, &q).map(|_| ()),
                None => Err("no KiCad libraries yet".into()),
            }
        };
        if let Ok(mut g) = q.lock() {
            g.finished = true;
            g.done = 1.0;
            if let Err(e) = r {
                g.error = Some(e);
            }
        }
    });
    p
}

// ---------------------------------------------------------------- the index

static INDEX: OnceLock<Mutex<Option<Arc<Index>>>> = OnceLock::new();

fn index_cell() -> &'static Mutex<Option<Arc<Index>>> {
    INDEX.get_or_init(|| Mutex::new(None))
}

fn index_path(root: &Root) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (root.symbols.to_string_lossy(), root.footprints.to_string_lossy(), 3u32, digikey_dir()).hash(&mut h);
    cache_dir().map(|d| d.join(format!("index-{:016x}.json", h.finish())))
}

/// The index of the current root, if it has been built (loaded from the cache the first time).
pub fn index() -> Option<Arc<Index>> {
    let root = root()?;
    let cell = index_cell();
    let label = ix_label(&root);
    if let Some(i) = cell.lock().ok()?.as_ref().filter(|i| i.label == label) {
        return Some(i.clone());
    }
    let text = std::fs::read(index_path(&root)?).ok()?;
    let ix: Index = serde_json::from_slice(&text).ok()?;
    if ix.label != label {
        return None;
    }
    let ix = Arc::new(ix);
    *cell.lock().ok()? = Some(ix.clone());
    Some(ix)
}

fn files_with(dir: &Path, ext: &str, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    v.sort();
    for q in v {
        if q.is_dir() {
            if depth > 0 {
                files_with(&q, ext, depth - 1, out);
            }
        } else if q.extension().is_some_and(|e| e == ext) {
            out.push(q);
        }
    }
}

fn prop(s: &Sx, k: &str) -> String {
    s.items()
        .iter()
        .filter(|x| x.head() == Some("property"))
        .find(|x| x.items().get(1).and_then(|a| a.atom()) == Some(k))
        .and_then(|x| x.items().get(2).and_then(|a| a.atom()).map(String::from))
        .unwrap_or_default()
}

fn has(s: &Sx, head: &str) -> bool {
    s.items().iter().any(|x| x.head() == Some(head))
}

/// The symbols of one parsed library file (or one symdir file) for the index.
fn index_symbols(root_sx: &Sx, lib: &str, file: &str, out: &mut Vec<SymEntry>) {
    for s in root_sx.items().iter().filter(|x| x.head() == Some("symbol")) {
        let Some(name) = s.items().get(1).and_then(|a| a.atom()) else { continue };
        let mut pins = std::collections::BTreeSet::new();
        for u in s.items().iter().filter(|x| x.head() == Some("symbol")) {
            for pin in u.items().iter().filter(|x| x.head() == Some("pin")) {
                if let Some(n) = pin.items().iter().find(|x| x.head() == Some("number")).and_then(|n| n.items().get(1)).and_then(|a| a.atom()) {
                    pins.insert(n.to_string());
                }
            }
        }
        let extends = s.items().iter().find(|x| x.head() == Some("extends")).and_then(|e| e.items().get(1)).and_then(|a| a.atom()).unwrap_or("").to_string();
        let desc = {
            let d = prop(s, "Description");
            if d.is_empty() { prop(s, "ki_description") } else { d }
        };
        out.push(SymEntry {
            lib: lib.to_string(),
            name: name.to_string(),
            desc,
            keys: prop(s, "ki_keywords"),
            fp: prop(s, "Footprint"),
            filters: prop(s, "ki_fp_filters").split_whitespace().map(String::from).collect(),
            pins: pins.len() as u32,
            power: has(s, "power"),
            datasheet: prop(s, "Datasheet"),
            file: file.to_string(),
            extends,
            digikey: prop(s, "Digi-Key_PN"),
            mpn: prop(s, "MPN"),
            manufacturer: prop(s, "Manufacturer"),
        });
    }
}

/// Index every symbol and footprint under `root` and save the index in the cache.
pub fn build_index(root: &Root, p: &Shared) -> Result<Arc<Index>, String> {
    let mut ix = Index { label: ix_label(root), ..Default::default() };
    set(p, "Indexing the KiCad symbols", 0.9);
    let mut files = vec![];
    files_with(&root.symbols, "kicad_sym", 1, &mut files);
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        let Ok(sx) = pcb_kicad::parse(&text) else { continue };
        let rel = f.strip_prefix(&root.symbols).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let lib = match f.parent().filter(|d| d.extension().is_some_and(|e| e == "kicad_symdir")) {
            Some(d) => d.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            None => f.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        };
        index_symbols(&sx, &lib, &rel, &mut ix.symbols);
    }
    // Digi-Key's library (KiCad 5 files), when it is there
    if let Some(d) = digikey_dir() {
        set(p, "Indexing Digi-Key's KiCad library", 0.93);
        let mut libs = vec![];
        files_with(&d.join("digikey-symbols"), "lib", 0, &mut libs);
        for f in &libs {
            let Ok(text) = std::fs::read_to_string(f) else { continue };
            let docs = std::fs::read_to_string(f.with_extension("dcm")).map(|t| crate::kicad_legacy::docs(&t)).unwrap_or_default();
            let Ok(sx) = crate::kicad_legacy::library(&text, &docs) else { continue };
            let lib = f.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let file = format!("{DK_FILE}{}", f.file_name().unwrap_or_default().to_string_lossy());
            index_symbols(&sx, &lib, &file, &mut ix.symbols);
        }
    }
    // a derived symbol's pins and filters are its base's
    let base: std::collections::HashMap<(String, String), (u32, Vec<String>)> = ix.symbols.iter().map(|s| ((s.lib.clone(), s.name.clone()), (s.pins, s.filters.clone()))).collect();
    for s in &mut ix.symbols {
        if !s.extends.is_empty() {
            if let Some((n, f)) = base.get(&(s.lib.clone(), s.extends.clone())) {
                if s.pins == 0 {
                    s.pins = *n;
                }
                if s.filters.is_empty() {
                    s.filters = f.clone();
                }
            }
        }
    }
    set(p, "Indexing the KiCad footprints", 0.95);
    let mut files = vec![];
    files_with(&root.footprints, "kicad_mod", 1, &mut files);
    let dk = digikey_dir();
    if let Some(d) = &dk {
        files_with(&d.join(format!("{DK_FOOTPRINTS}.pretty")), "kicad_mod", 0, &mut files);
    }
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        let Ok(sx) = pcb_kicad::parse(&text) else { continue };
        let lib = f.parent().and_then(|d| d.file_stem()).unwrap_or_default().to_string_lossy().into_owned();
        let atom = |h: &str| sx.items().iter().find(|x| x.head() == Some(h)).and_then(|x| x.items().get(1)).and_then(|a| a.atom()).unwrap_or("").to_string();
        let mut pads = std::collections::BTreeSet::new();
        let mut smd = false;
        for pad in sx.items().iter().filter(|x| x.head() == Some("pad")) {
            let it = pad.items();
            let num = it.get(1).and_then(|a| a.atom()).unwrap_or("");
            let kind = it.get(2).and_then(|a| a.atom()).unwrap_or("");
            smd |= kind == "smd";
            if !num.is_empty() && kind != "np_thru_hole" {
                pads.insert(num.to_string());
            }
        }
        let name = sx.items().get(1).and_then(|a| a.atom()).map(String::from).unwrap_or_else(|| f.file_stem().unwrap_or_default().to_string_lossy().into_owned());
        ix.footprints.push(FpEntry { lib, name, desc: atom("descr"), tags: atom("tags"), pads: pads.len() as u32, smd, model: pcb_kicad::model_of(&sx).map(|m| m.file).unwrap_or_default() });
    }
    if ix.symbols.is_empty() && ix.footprints.is_empty() {
        return Err(format!("no KiCad symbols or footprints in {}", root.symbols.parent().unwrap_or(&root.symbols).display()));
    }
    if let Some(path) = index_path(root) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&path, serde_json::to_vec(&ix).unwrap_or_default());
    }
    let ix = Arc::new(ix);
    if let Ok(mut c) = index_cell().lock() {
        *c = Some(ix.clone());
    }
    Ok(ix)
}

/// The index, building it (or downloading the libraries first) when there is none: blocking,
/// for the agent and scripts.
pub fn ensure() -> Result<Arc<Index>, String> {
    if let Some(i) = index() {
        return Ok(i);
    }
    let p: Shared = Arc::default();
    match root() {
        Some(r) => build_index(&r, &p),
        None => {
            download(&p)?;
            index().ok_or_else(|| "the KiCad libraries downloaded but could not be indexed".to_string())
        }
    }
}

/// A search result for the agent.
pub fn sym_json(s: &SymEntry) -> serde_json::Value {
    let mut v = serde_json::json!({"key": s.key(), "description": s.desc, "keywords": s.keys, "footprint": s.fp, "footprint_filters": s.filters, "pins": s.pins, "power": s.power, "datasheet": s.datasheet});
    if !s.digikey.is_empty() {
        v["digikey"] = s.digikey.clone().into();
        v["mpn"] = s.mpn.clone().into();
        v["manufacturer"] = s.manufacturer.clone().into();
    }
    v
}

pub fn fp_json(f: &FpEntry) -> serde_json::Value {
    serde_json::json!({"key": f.key(), "description": f.desc, "tags": f.tags, "pads": f.pads, "smd": f.smd, "model": !f.model.is_empty()})
}

// ---------------------------------------------------------------- searching

/// How well `q`'s words match: every word must be found; the name counts most.
fn score(q: &[String], name: &str, lib: &str, rest: &[&str]) -> Option<u32> {
    let (n, l) = (name.to_lowercase(), lib.to_lowercase());
    let rest: Vec<String> = rest.iter().map(|r| r.to_lowercase()).collect();
    let mut total = 0;
    for w in q {
        let s = if n == *w {
            100
        } else if n.starts_with(w.as_str()) {
            60
        } else if n.contains(w.as_str()) {
            40
        } else if l.contains(w.as_str()) {
            25
        } else if rest.iter().any(|r| r.split(|c: char| !c.is_alphanumeric()).any(|t| t == w)) {
            20
        } else if rest.iter().any(|r| r.contains(w.as_str())) {
            10
        } else {
            return None;
        };
        total += s;
    }
    // shorter names first among equals (R before R_Small_US)
    Some(total * 100 + 99u32.saturating_sub(name.len() as u32))
}

fn words(q: &str) -> Vec<String> {
    q.split_whitespace().map(|w| w.to_lowercase()).collect()
}

/// Symbols matching `q` (all of them, in library order, when `q` is empty), best first.
pub fn search_symbols<'a>(ix: &'a Index, q: &str, power_only: bool, limit: usize) -> Vec<&'a SymEntry> {
    let w = words(q);
    let mut v: Vec<(u32, &SymEntry)> = ix
        .symbols
        .iter()
        // pinless symbols (mounting screws, logos, BOM-only items) can't be placed in rCAD
        .filter(|s| s.pins > 0 && (!power_only || s.power))
        .filter_map(|s| {
            if w.is_empty() {
                return Some((0, s));
            }
            // "Device:R" finds that symbol
            if let Some((l, n)) = q.trim().split_once(':') {
                return (s.lib.eq_ignore_ascii_case(l) && s.name.to_lowercase().starts_with(&n.to_lowercase())).then_some((u32::MAX - s.name.len() as u32, s));
            }
            score(&w, &s.name, &s.lib, &[&s.desc, &s.keys, &s.digikey, &s.mpn, &s.manufacturer]).map(|k| (k, s))
        })
        .collect();
    if !w.is_empty() {
        v.sort_by(|a, b| b.0.cmp(&a.0));
    }
    v.into_iter().take(limit).map(|x| x.1).collect()
}

/// Footprints matching `q` and, when given, a symbol's filters and pin count, best first.
pub fn search_footprints<'a>(ix: &'a Index, q: &str, filters: &[String], pins: Option<u32>, limit: usize) -> Vec<&'a FpEntry> {
    let w = words(q);
    let mut v: Vec<(u32, &FpEntry)> = ix
        .footprints
        .iter()
        .filter(|f| f.pads > 0 && filter_ok(filters, f) && pins.is_none_or(|n| n == 0 || f.pads == n))
        .filter_map(|f| if w.is_empty() { Some((0, f)) } else { score(&w, &f.name, &f.lib, &[&f.desc, &f.tags]).map(|k| (k, f)) })
        .collect();
    if !w.is_empty() {
        v.sort_by(|a, b| b.0.cmp(&a.0));
    } else if !filters.is_empty() {
        // a symbol's filters list its usual package first (SOIC before LFCSP for a TL072)
        v.sort_by_key(|(_, f)| (filters.iter().position(|p| filter_ok(std::slice::from_ref(p), f)).unwrap_or(usize::MAX), f.name.contains("HandSolder") as u8));
    }
    v.into_iter().take(limit).map(|x| x.1).collect()
}

/// KiCad footprint filters: `Name*` matches the footprint name, `Lib:Name*` library and name.
pub fn filter_ok(filters: &[String], f: &FpEntry) -> bool {
    filters.is_empty()
        || filters.iter().any(|p| match p.split_once(':') {
            Some((l, n)) => pcb_lib::glob(l, &f.lib) && pcb_lib::glob(n, &f.name),
            None => pcb_lib::glob(p, &f.name),
        })
}

pub fn find_symbol<'a>(ix: &'a Index, key: &str) -> Option<&'a SymEntry> {
    let (l, n) = key.split_once(':')?;
    ix.symbols.iter().find(|s| s.lib == l && s.name == n)
}

pub fn find_footprint<'a>(ix: &'a Index, key: &str) -> Option<&'a FpEntry> {
    match key.split_once(':') {
        Some((l, n)) => ix.footprints.iter().find(|f| f.lib == l && f.name == n),
        None => ix.footprints.iter().find(|f| f.name == key),
    }
}

// ---------------------------------------------------------------- loading parts

/// The symbol `s` as an rCAD library symbol, with what it extends.
pub fn load_symbol(root: &Root, ix: &Index, s: &SymEntry) -> Result<LibSymbol, String> {
    if let Some(file) = s.file.strip_prefix(DK_FILE) {
        let d = digikey_dir().ok_or("Digi-Key's KiCad library isn't downloaded")?;
        let path = d.join("digikey-symbols").join(file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let docs = std::fs::read_to_string(path.with_extension("dcm")).map(|t| crate::kicad_legacy::docs(&t)).unwrap_or_default();
        let sx = crate::kicad_legacy::library(&text, &docs)?;
        let mut notes = vec![];
        let mut v = pcb_kicad::symbols(&sx, &s.lib, &mut notes)?;
        let mut sym = v.drain(..).find(|x| x.name() == s.name).ok_or_else(|| if notes.is_empty() { format!("{} could not be read", s.key()) } else { notes.join("; ") })?;
        sym.source = DK_CREDIT.into();
        return Ok(sym);
    }
    let mut chain = vec![s.clone()];
    while let Some(b) = chain.last().filter(|c| !c.extends.is_empty()).and_then(|c| ix.symbols.iter().find(|x| x.lib == c.lib && x.name == c.extends)) {
        if chain.len() > 8 || chain.iter().any(|c| c.name == b.name) {
            break;
        }
        chain.push(b.clone());
    }
    let mut items = vec![Sx::Atom("kicad_symbol_lib".into())];
    let mut read: Vec<String> = vec![];
    for c in &chain {
        if read.contains(&c.file) {
            continue;
        }
        read.push(c.file.clone());
        let path = root.symbols.join(&c.file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let sx = pcb_kicad::parse(&text)?;
        for x in sx.items().iter().filter(|x| x.head() == Some("symbol")) {
            let n = x.items().get(1).and_then(|a| a.atom()).unwrap_or("");
            if chain.iter().any(|c| c.name == n) {
                items.push(x.clone());
            }
        }
    }
    let mut notes = vec![];
    let mut v = pcb_kicad::symbols(&Sx::List(items), &s.lib, &mut notes)?;
    let mut sym = v.drain(..).find(|x| x.name() == s.name).ok_or_else(|| if notes.is_empty() { format!("{} could not be read", s.key()) } else { notes.join("; ") })?;
    sym.source = CREDIT.into();
    Ok(sym)
}

/// The footprint `lib:name` from the library.
pub fn load_footprint(root: &Root, f: &FpEntry) -> Result<Footprint, String> {
    let dir = match (f.lib == DK_FOOTPRINTS).then(digikey_dir).flatten() {
        Some(d) => d,
        None => root.footprints.clone(),
    };
    let path = dir.join(format!("{}.pretty", f.lib)).join(format!("{}.kicad_mod", f.name));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut notes = vec![];
    let mut fp = pcb_kicad::footprint(&pcb_kicad::parse(&text)?, &mut notes)?;
    fp.source = if f.lib == DK_FOOTPRINTS { DK_CREDIT.into() } else { CREDIT.into() };
    Ok(fp)
}

/// Load symbol `key` with footprint `fp` (a "Lib:Name" key; empty for the symbol's own), add both
/// to `pcb`'s library and register them, ready for `Pcb::add_part` with the symbol key.
pub fn add_symbol(pcb: &mut crate::pcb::Pcb, key: &str, fp: &str) -> Result<Vec<String>, String> {
    let root = root().ok_or("the KiCad libraries aren't downloaded yet")?;
    let ix = index().ok_or("the KiCad libraries aren't indexed yet")?;
    let s = find_symbol(&ix, key).ok_or_else(|| format!("no symbol {key} in the KiCad libraries"))?;
    let sym = load_symbol(&root, &ix, s)?;
    let mut want = if fp.is_empty() { sym.footprint.clone() } else { fp.to_string() };
    if want.is_empty() {
        // no default (connectors, generic parts): the first footprint its filters accept with a
        // pad per pin, as KiCad's footprint chooser lists them
        let mut pick = search_footprints(&ix, "", &s.filters, Some(s.pins), 1).first().map(|f| f.key());
        if pick.is_none() && !s.filters.is_empty() {
            pick = search_footprints(&ix, "", &s.filters, None, 1).first().map(|f| f.key());
        }
        want = pick.unwrap_or_default();
    }
    let mut im = pcb_kicad::Imported::default();
    if !want.is_empty() {
        match find_footprint(&ix, &want) {
            Some(f) => im.footprints.push(load_footprint(&root, f)?),
            None => im.notes.push(format!("footprint {want} is not in the KiCad libraries")),
        }
    }
    let mut sym = sym;
    if let Some(f) = im.footprints.first() {
        sym.footprint = f.name.clone();
    }
    im.symbols.push(sym);
    Ok(pcb_kicad::add_to(pcb, im))
}

/// Give part `i` footprint `fp` ("Lib:Name") from the KiCad libraries.
pub fn set_footprint(pcb: &mut crate::pcb::Pcb, i: usize, fp: &str) -> Result<String, String> {
    let root = root().ok_or("the KiCad libraries aren't downloaded yet")?;
    let ix = index().ok_or("the KiCad libraries aren't indexed yet")?;
    let f = find_footprint(&ix, fp).ok_or_else(|| format!("no footprint {fp} in the KiCad libraries"))?;
    let mut footprint = load_footprint(&root, f)?;
    if pcb_lib::is_builtin_footprint(&footprint.name) {
        footprint.name = format!("{}_KiCad", footprint.name);
    }
    let name = footprint.name.clone();
    pcb_lib::register_footprints(std::slice::from_ref(&footprint));
    pcb.footprints.retain(|g| g.name != name);
    pcb.footprints.push(footprint);
    let part = pcb.parts.get_mut(i).ok_or("no such part")?;
    part.footprint = name.clone();
    Ok(name)
}

// ---------------------------------------------------------------- 3D models

/// The STEP file of model `file` (`Lib.3dshapes/Name`): from the libraries' 3D folder or the
/// cache, else downloaded into the cache (when `fetch`).
pub fn model_step(file: &str, fetch: bool) -> Result<Vec<u8>, String> {
    if file.contains("..") || file.starts_with('/') {
        return Err(format!("odd model path {file}"));
    }
    let rel = format!("{file}.step");
    let mut places = vec![];
    if let Some(m) = root().and_then(|r| r.models) {
        places.push(m.join(&rel));
    }
    let cached = download_dir().map(|d| d.join("3dmodels").join(&rel));
    places.extend(cached.clone());
    for p in &places {
        if let Ok(b) = std::fs::read(p) {
            return Ok(b);
        }
    }
    if !fetch {
        return Err(format!("{rel} isn't downloaded"));
    }
    let url = format!("{RAW}kicad-packages3D/-/raw/{VERSION}/{}", rel.split('/').map(urlencode).collect::<Vec<_>>().join("/"));
    let b = get(&url, 64 << 20, &mut |_, _| {})?;
    if !b.starts_with(b"ISO-10303-21") {
        return Err(format!("{rel}: the download is not a STEP file"));
    }
    if let Some(c) = cached {
        if let Some(d) = c.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&c, &b);
    }
    Ok(b)
}

fn urlencode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o += &format!("%{b:02X}");
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(lib: &str, name: &str) -> FpEntry {
        FpEntry { lib: lib.into(), name: name.into(), pads: 2, ..Default::default() }
    }

    #[test]
    fn filters_and_search() {
        let ix = Index {
            label: "t".into(),
            symbols: vec![
                SymEntry { lib: "Device".into(), name: "R".into(), desc: "Resistor".into(), keys: "R res resistor".into(), pins: 2, filters: vec!["R_*".into()], file: "x".into(), ..Default::default() },
                SymEntry { lib: "Device".into(), name: "R_Small_US".into(), desc: "Resistor, small US symbol".into(), pins: 2, file: "x".into(), ..Default::default() },
                SymEntry { lib: "power".into(), name: "GND".into(), power: true, pins: 1, file: "y".into(), ..Default::default() },
            ],
            footprints: vec![entry("Resistor_SMD", "R_0603_1608Metric"), entry("Capacitor_SMD", "C_0603_1608Metric"), entry("Resistor_THT", "R_Axial_DIN0207_L6.3mm_D2.5mm_P10.16mm_Horizontal")],
        };
        let r = search_symbols(&ix, "resistor", false, 10);
        assert_eq!(r[0].name, "R");
        assert_eq!(search_symbols(&ix, "Device:R_S", false, 10)[0].name, "R_Small_US");
        assert_eq!(search_symbols(&ix, "", true, 10).len(), 1);
        let f = search_footprints(&ix, "", &["R_*".into()], Some(2), 10);
        assert_eq!(f.len(), 2);
        let f = search_footprints(&ix, "0603", &["Resistor_SMD:R_*".into()], Some(2), 10);
        assert_eq!(f.len(), 1);
        assert!(search_footprints(&ix, "", &["R_*".into()], Some(3), 10).is_empty());
    }

    #[test]
    fn model_paths() {
        let sx = pcb_kicad::parse(r#"(footprint "X" (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu"))
            (model "${KICAD10_3DMODEL_DIR}/Resistor_SMD.3dshapes/R_0603_1608Metric.wrl" (offset (xyz 0 0 0.5)) (scale (xyz 1 1 1)) (rotate (xyz -90 0 90))))"#)
        .unwrap();
        let m = pcb_kicad::model_of(&sx).unwrap();
        assert_eq!(m.file, "Resistor_SMD.3dshapes/R_0603_1608Metric");
        assert_eq!(m.offset, [0.0, 0.0, 0.5]);
        assert_eq!(m.rotate, [-90.0, 0.0, 90.0]);
        let old = pcb_kicad::parse(r#"(module "Y" (model C:/libs/Package_SO.3dshapes/SOIC-8.step (at (xyz 0.1 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))"#).unwrap();
        let m = pcb_kicad::model_of(&old).unwrap();
        assert_eq!(m.file, "Package_SO.3dshapes/SOIC-8");
        assert!((m.offset[0] - 2.54).abs() < 1e-9);
    }

    /// Downloads the release (about 60 MB): `RCAD_KICAD_CACHE=/tmp/k cargo test download_release -- --ignored`.
    #[test]
    #[ignore]
    fn download_release() {
        let p: Shared = Arc::default();
        download(&p).unwrap();
        let ix = index().unwrap();
        assert!(ix.symbols.len() > 15000 && ix.footprints.len() > 10000, "{} {}", ix.symbols.len(), ix.footprints.len());
        let b = model_step("Resistor_SMD.3dshapes/R_0603_1608Metric", true).unwrap();
        assert!(b.starts_with(b"ISO-10303-21"));
    }

    /// Every symbol and footprint of the libraries loads (slow: `-- --ignored load_everything`).
    #[test]
    #[ignore]
    fn load_everything() {
        let root = root().expect("libraries");
        let ix = index().expect("index");
        let mut bad_s = vec![];
        for s in &ix.symbols {
            if let Err(e) = load_symbol(&root, &ix, s) {
                bad_s.push(format!("{}: {e}", s.key()));
            }
        }
        let mut bad_f = vec![];
        for f in &ix.footprints {
            if let Err(e) = load_footprint(&root, f) {
                bad_f.push(format!("{}: {e}", f.key()));
            }
        }
        eprintln!("symbols {} bad {}; footprints {} bad {}", ix.symbols.len(), bad_s.len(), ix.footprints.len(), bad_f.len());
        for b in bad_s.iter().take(15).chain(bad_f.iter().filter(|b| !b.contains("no copper pads")).take(15)) {
            eprintln!("  {b}");
        }
    }

    /// KiCad parts on a board, then the 3D board with their STEP models (needs the libraries and
    /// the network for the models: `-- --ignored kicad_parts_in_3d`).
    #[test]
    #[ignore]
    fn kicad_parts_in_3d() {
        let mut ws = crate::workspace::Workspace::default();
        let mut p = crate::pcb::tests::led_board();
        let ix = ensure().unwrap();
        let n0 = p.parts.len();
        for (k, (sym, fp)) in [("Device:R", "Resistor_SMD:R_0603_1608Metric"), ("Regulator_Linear:AMS1117-3.3", ""), ("Amplifier_Current:INA281A2", "")].into_iter().enumerate() {
            add_symbol(&mut p, sym, fp).unwrap();
            let i = p.add_part(sym, None, None, [100.0, 10.0 * k as f64]).unwrap();
            p.parts[i].at = [5.0 + 6.0 * k as f64, 5.0];
        }
        assert!(find_symbol(&ix, "Device:R").is_some());
        assert_eq!(p.parts[n0].footprint, "R_0603_1608Metric");
        assert_eq!(p.parts[n0 + 1].footprint, "SOT-223-3_TabPin2");
        assert_eq!(p.parts[n0 + 2].footprint, "SOT-23-5_KiCad");
        assert!(p.parts[n0].description.contains("Resistor"), "{}", p.parts[n0].description);
        ws.pcbs.push(p);
        let si = crate::pcb_export::to_studio(&mut ws, 0).unwrap();
        let doc = &ws.studios[si].doc;
        let script = doc.custom_features.iter().find(|d| d.source.contains("R_0603_1608Metric.step")).expect("the resistor's model");
        assert!(script.source.contains("step_data("));
        let r = crate::kernel::rebuild(doc, &[], |_| {});
        for f in &r.features {
            assert!(f.error.is_none(), "{}: {:?}", f.id, f.error);
        }
        // the resistor's body sits on the board top (1.6 mm) at its spot, about 1.6 x 0.8 mm
        let fid = doc.features.iter().find(|f| f.name.starts_with(&ws.pcbs[0].parts[n0].reference)).unwrap().id;
        let b = r.bodies.iter().find(|b| b.id.0 == fid).expect("resistor body");
        let (lo, hi) = (b.bbox[0], b.bbox[1]);
        assert!((lo[2] - 1.6).abs() < 0.05 && hi[2] > 1.8 && hi[2] < 2.3, "{lo:?} {hi:?}");
        assert!(((lo[0] + hi[0]) / 2.0 - 5.0).abs() < 0.05 && (hi[0] - lo[0] - 1.6).abs() < 0.15, "{lo:?} {hi:?}");
    }

    /// The downloaded libraries, when a test machine has them (set RCAD_KICAD_DIR or download
    /// once from the PCB tab): every symbol of Device loads, and its default footprints exist.
    #[test]
    fn real_library_if_present() {
        let Some(root) = root() else { return };
        let p: Shared = Arc::default();
        let Some(ix) = index().or_else(|| build_index(&root, &p).ok()) else { return };
        let r = find_symbol(&ix, "Device:R").expect("Device:R");
        let sym = load_symbol(&root, &ix, r).unwrap();
        assert_eq!(sym.pins.len(), 2);
        let f = find_footprint(&ix, "Resistor_SMD:R_0603_1608Metric").expect("R_0603");
        let fp = load_footprint(&root, f).unwrap();
        assert_eq!(fp.pin_numbers(), vec!["1", "2"]);
        assert!(fp.model.is_some());
        let mut bad = vec![];
        for s in ix.symbols.iter().filter(|s| s.lib == "Device" || s.lib == "Amplifier_Operational") {
            if let Err(e) = load_symbol(&root, &ix, s) {
                bad.push(format!("{}: {e}", s.key()));
            }
        }
        assert!(bad.len() * 50 < ix.symbols.iter().filter(|s| s.lib == "Device" || s.lib == "Amplifier_Operational").count(), "{bad:?}");
    }
}
