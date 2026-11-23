//! The link with OrcaSlicer (Jonas's fork, JonasMerrell/OrcaSlicer).
//!
//! "Print in OrcaSlicer" writes each part to print as its own STEP file under the document's
//! folder (`.rcad-print/<studio>/<part>.step`) and hands the files to the slicer with
//! `--single-instance`, so they open in the slicer window that is already up. Next to each file
//! goes `<file>.rcad.json`, naming the document and part, this program, and the local address
//! rCAD listens on: the fork reads it to offer "Edit in rCAD" on the object, which asks this
//! rCAD (or a new one) to show the part.
//!
//! The fork reloads a file it already has in place (placement and settings kept), so sending a
//! part again updates it. With "Keep the slicer updated" on, rCAD does that by itself whenever
//! a part it sent is rebuilt with a change.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::app::{App, ToastKind};
use crate::doc::BodyId;

/// The folder (next to the document) the files sent to the slicer go in.
pub const DIR: &str = ".rcad-print";

/// A part sent to the slicer this session.
#[derive(Clone, Debug)]
pub struct Sent {
    pub studio: usize,
    pub body: BodyId,
    pub file: PathBuf,
    /// Hash of the Part Studio's document when the file was written.
    pub doc_hash: u64,
}

/// A request from the slicer to show a part ("Edit in rCAD", or the command line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditRequest {
    pub document: String,
    pub studio: String,
    pub part: String,
    pub file: String,
}

#[derive(Default)]
pub struct Link {
    /// 127.0.0.1:port of the listener, once it runs.
    addr: Option<String>,
    requests: Arc<Mutex<Vec<EditRequest>>>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    pub sent: Vec<Sent>,
    /// Parts being written for the slicer: (export marker, parts, whether the user asked).
    pub pending: Option<(String, Vec<Sent>, bool)>,
    /// Parts that changed while a send was in flight, to send when it is done.
    pub resend: Vec<Sent>,
    /// A part to select once its Part Studio is built.
    waiting: Option<EditRequest>,
    next: u64,
}

// ------------------------------------------------------------------ finding the slicer

/// The slicer program: the one set in the 3D printing settings, else the first found where
/// OrcaSlicer usually is (the fork's own build folder first).
pub fn find_slicer(setting: &str) -> Option<PathBuf> {
    let setting = setting.trim();
    if !setting.is_empty() {
        return Some(PathBuf::from(setting));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let mut c: Vec<PathBuf> = vec![];
    if let Some(h) = &home {
        c.push(h.join("OrcaSlicer/build/package/bin/orca-slicer"));
        c.push(h.join("OrcaSlicer/build/package/orca-slicer"));
        c.push(h.join("OrcaSlicer/build/src/Release/orca-slicer.exe"));
        c.push(h.join("OrcaSlicer/build/OrcaSlicer/orca-slicer.exe"));
        c.push(h.join("Applications/OrcaSlicer.AppImage"));
    }
    for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(p) = std::env::var_os(var) {
            c.push(PathBuf::from(&p).join("OrcaSlicer").join("orca-slicer.exe"));
            c.push(PathBuf::from(&p).join("Programs").join("OrcaSlicer").join("orca-slicer.exe"));
        }
    }
    c.push("/Applications/OrcaSlicer.app/Contents/MacOS/OrcaSlicer".into());
    if let Some(found) = c.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    let names: &[&str] = if cfg!(windows) { &["orca-slicer.exe", "OrcaSlicer.exe"] } else { &["orca-slicer", "OrcaSlicer", "orcaslicer"] };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

/// File-name-safe text (names keep spaces and most punctuation, not path separators).
pub fn safe_name(s: &str) -> String {
    let t: String = s.trim().chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let t = t.trim_matches('.').trim().to_string();
    if t.is_empty() {
        "part".into()
    } else {
        t
    }
}

/// Where the files for the slicer go: next to the document, or in the temp folder while it is
/// unsaved (the slicer then reloads them only for this session).
pub fn print_dir(doc: Option<&Path>) -> PathBuf {
    match doc.and_then(|p| p.parent()) {
        Some(d) if !d.as_os_str().is_empty() => d.join(DIR),
        _ => std::env::temp_dir().join(format!("rcad-print-{}", std::process::id())),
    }
}

/// The sidecar the fork reads to link an object back to rCAD.
pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".rcad.json");
    PathBuf::from(s)
}

pub fn sidecar_json(app: Option<&Path>, document: Option<&Path>, studio: &str, part: &str, link: Option<&str>) -> serde_json::Value {
    let s = |p: Option<&Path>| p.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    serde_json::json!({
        "format": "rcad-link-1",
        "app": s(app),
        "document": s(document),
        "studio": studio,
        "part": part,
        "link": link.unwrap_or(""),
    })
}

fn doc_hash(d: &crate::doc::Document) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(d).unwrap_or_default().hash(&mut h);
    h.finish()
}

// ------------------------------------------------------------------ the link listener

/// Decode one `%xx` / `+` encoded query value.
pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && hex(b[i + 1]).is_some() && hex(b[i + 2]).is_some() => {
                out.push(hex(b[i + 1]).unwrap() * 16 + hex(b[i + 2]).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The edit request in an HTTP request line ("GET /edit?document=…&part=… HTTP/1.1").
pub fn parse_request_line(line: &str) -> Option<EditRequest> {
    let mut it = line.split_whitespace();
    if it.next()? != "GET" {
        return None;
    }
    let target = it.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/edit" {
        return None;
    }
    let mut r = EditRequest::default();
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = url_decode(v);
        match k {
            "document" => r.document = v,
            "studio" => r.studio = v,
            "part" => r.part = v,
            "file" => r.file = v,
            _ => {}
        }
    }
    Some(r)
}

impl Link {
    /// Start listening for the slicer (once); returns the address.
    pub fn listen(&mut self) -> Option<String> {
        if self.addr.is_some() {
            return self.addr.clone();
        }
        let l = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        let addr = l.local_addr().ok()?.to_string();
        let (reqs, ctx) = (self.requests.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
                let mut line = String::new();
                let _ = BufReader::new(&s).read_line(&mut line);
                let (status, body) = match parse_request_line(&line) {
                    Some(r) => {
                        if let Ok(mut q) = reqs.lock() {
                            q.push(r);
                        }
                        if let Some(c) = ctx.lock().ok().and_then(|c| c.clone()) {
                            c.request_repaint();
                        }
                        ("200 OK", "ok")
                    }
                    None => ("404 Not Found", "not found"),
                };
                let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        self.addr = Some(addr);
        self.addr.clone()
    }

    pub fn push_request(&self, r: EditRequest) {
        if let Ok(mut q) = self.requests.lock() {
            q.push(r);
        }
    }

    fn take_requests(&self) -> Vec<EditRequest> {
        self.requests.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
    }
}

// ------------------------------------------------------------------ sending

/// The parts "Print in OrcaSlicer" sends: in a Part Studio the selected parts, else every part
/// in sight; in an assembly the parts of the selected instances, else of every instance in
/// sight (each part once).
pub fn parts_to_send(app: &App) -> Vec<(usize, BodyId)> {
    let mut out: Vec<(usize, BodyId)> = vec![];
    let mut add = |x: (usize, BodyId)| {
        if !out.contains(&x) {
            out.push(x);
        }
    };
    match app.tab {
        crate::workspace::Tab::Assembly(ai) => {
            let Some(a) = app.ws.assemblies.get(ai) else { return vec![] };
            let sel = &app.asm.sel;
            for x in a.instances.iter().filter(|x| !x.hidden && (sel.is_empty() || sel.contains(&x.id))) {
                for l in crate::assembly::leaves(&app.ws.assemblies, x) {
                    add((l.studio, l.body));
                }
            }
        }
        _ => {
            let s = app.cur_studio;
            let picked: Vec<BodyId> = app.picks.iter().filter_map(|r| if let crate::doc::Ref::Body { body } = r { Some(*body) } else { None }).collect();
            for b in &app.result.bodies {
                let shown = !app.part_props(b.id).hidden;
                if (picked.is_empty() && shown) || picked.contains(&b.id) {
                    add((s, b.id));
                }
            }
        }
    }
    out
}

fn part_name(app: &App, studio: usize, body: BodyId) -> String {
    app.studio_doc(studio)
        .and_then(|d| d.parts.get(&crate::doc::body_key(body)))
        .and_then(|p| p.name.clone())
        .or_else(|| app.studio_bodies(studio).iter().find(|b| b.id == body).map(|b| b.name.clone()))
        .unwrap_or_else(|| "Part".into())
}

fn studio_name(app: &App, studio: usize) -> String {
    app.ws.studios.get(studio).map(|s| s.name.clone()).unwrap_or_else(|| format!("Part Studio {}", studio + 1))
}

/// "Print in OrcaSlicer": write the parts and open them in the slicer.
pub fn print_in_orca(app: &mut App) {
    let parts = parts_to_send(app);
    if parts.is_empty() {
        app.flash("There are no parts to print", ToastKind::Info);
        return;
    }
    if find_slicer(&app.print.orca_path).is_none() {
        app.flash("OrcaSlicer wasn't found: set where it is in File > 3D printing settings", ToastKind::Error);
        return;
    }
    let dir = print_dir(app.path.as_deref());
    // one file per part, in a folder per Part Studio; names that clash get a number
    let mut used: HashMap<PathBuf, usize> = HashMap::new();
    let mut sends = vec![];
    for (studio, body) in parts {
        // a part sent before keeps its file, so the slicer reloads it instead of adding a copy
        if let Some(s) = app.orca.sent.iter().find(|s| s.studio == studio && s.body == body) {
            used.insert(s.file.clone(), 1);
            sends.push(Sent { doc_hash: 0, ..s.clone() });
            continue;
        }
        let sdir = dir.join(safe_name(&studio_name(app, studio)));
        let stem = safe_name(&part_name(app, studio, body));
        let mut file = sdir.join(format!("{stem}.step"));
        let mut n = 1;
        while used.contains_key(&file) || app.orca.sent.iter().any(|s| s.file == file) {
            n += 1;
            file = sdir.join(format!("{stem} {n}.step"));
        }
        used.insert(file.clone(), 1);
        sends.push(Sent { studio, body, file, doc_hash: 0 });
    }
    start_send(app, sends, true);
}

/// Write `sends` as STEP (in the kernel process); `finish_send` hands them to the slicer.
pub fn start_send(app: &mut App, mut sends: Vec<Sent>, asked: bool) {
    if app.orca.pending.is_some() {
        // one write at a time: the newest wins
        for s in sends {
            app.orca.resend.retain(|r| r.file != s.file);
            app.orca.resend.push(s);
        }
        return;
    }
    let dir = print_dir(app.path.as_deref());
    if std::fs::create_dir_all(&dir).is_ok() {
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "# rCAD's files for the slicer: made again from the parts, not kept in version control\n*\n");
        }
    }
    let mut items = vec![];
    for s in &mut sends {
        if let Some(p) = s.file.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        s.doc_hash = app.studio_doc(s.studio).map(doc_hash).unwrap_or(0);
        let item: crate::kernel::AsmExportItem = (s.studio, s.body, [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0], part_name(app, s.studio, s.body));
        items.push((item, s.file.to_string_lossy().into_owned()));
    }
    app.orca.next += 1;
    let marker = format!("\u{1}orca-send-{}", app.orca.next);
    let docs = app.studio_docs();
    app.kernel.set_studios(docs);
    app.kernel.export_parts(items, marker.clone());
    if asked {
        app.flash_info("Sending to OrcaSlicer…", None);
    }
    app.orca.pending = Some((marker, sends, asked));
}

/// The kernel wrote the files of a send: link them and open them in the slicer.
pub fn finish_send(app: &mut App, result: Result<Option<String>, String>) {
    let Some((_, sends, asked)) = app.orca.pending.take() else { return };
    match result {
        Err(e) => app.flash(format!("Could not write the parts for OrcaSlicer: {e}"), ToastKind::Error),
        Ok(_) => {
            let link = app.orca.listen();
            let exe = std::env::current_exe().ok();
            let document = app.path.as_ref().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
            let mut files = vec![];
            for s in &sends {
                let side = sidecar_json(exe.as_deref(), document.as_deref(), &studio_name(app, s.studio), &part_name(app, s.studio, s.body), link.as_deref());
                let _ = std::fs::write(sidecar_path(&s.file), serde_json::to_string_pretty(&side).unwrap_or_default());
                files.push(s.file.clone());
                match app.orca.sent.iter_mut().find(|x| x.file == s.file) {
                    Some(x) => *x = s.clone(),
                    None => app.orca.sent.push(s.clone()),
                }
            }
            match launch(&app.print.orca_path, &files) {
                Ok(()) if asked => {
                    let n = files.len();
                    let what = if n == 1 { "1 part".to_string() } else { format!("{n} parts") };
                    let more = if app.print.orca_live { ", and keeps it updated as you edit" } else { "" };
                    app.flash_info(format!("Sent {what} to OrcaSlicer{more}"), Some(std::time::Duration::from_secs(4)));
                }
                Ok(()) => app.flash_info("Updated the part in OrcaSlicer", Some(std::time::Duration::from_secs(2))),
                Err(e) => app.flash(e, ToastKind::Error),
            }
        }
    }
    if !app.orca.resend.is_empty() {
        let again = std::mem::take(&mut app.orca.resend);
        start_send(app, again, false);
    }
}

/// Open files in the slicer window that is up (or a new one).
pub fn launch(setting: &str, files: &[PathBuf]) -> Result<(), String> {
    let exe = find_slicer(setting).ok_or("OrcaSlicer wasn't found: set where it is in File > 3D printing settings")?;
    let mut c = std::process::Command::new(&exe);
    #[cfg(target_os = "macos")]
    if exe.extension().is_some_and(|e| e == "app") {
        c = std::process::Command::new("open");
        c.arg("-a").arg(&exe).arg("--args");
    }
    c.arg("--single-instance").args(files);
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    c.spawn().map(|_| ()).map_err(|e| format!("Could not start OrcaSlicer ({}): {e}", exe.display()))
}

/// After Part Studio `studio` was rebuilt: send its parts the slicer has again, if they changed.
pub fn after_rebuild(app: &mut App, studio: usize) {
    if !app.print.orca_live || app.orca.sent.iter().all(|s| s.studio != studio) {
        return;
    }
    let Some(h) = app.studio_doc(studio).map(doc_hash) else { return };
    let bodies: Vec<BodyId> = app.studio_bodies(studio).iter().map(|b| b.id).collect();
    let changed: Vec<Sent> = app.orca.sent.iter().filter(|s| s.studio == studio && s.doc_hash != h && bodies.contains(&s.body)).cloned().collect();
    if !changed.is_empty() {
        start_send(app, changed, false);
    }
}

// ------------------------------------------------------------------ showing a part

/// Once a frame: keep the window's context for the listener, and show the parts the slicer asked for.
pub fn frame(app: &mut App, ctx: &egui::Context) {
    if let Ok(mut c) = app.orca.ctx.lock() {
        if c.is_none() {
            *c = Some(ctx.clone());
        }
    }
    for r in app.orca.take_requests() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        open_for(app, &r);
    }
    // the part is picked once the Part Studio it is in has been built
    if app.orca.waiting.is_some() && app.kernel.state == crate::worker::KernelState::Ready && !app.dirty_model {
        let r = app.orca.waiting.take().unwrap();
        pick(app, &r);
    }
}

/// The first half of showing a part: open its document (when another one is open) and its
/// Part Studio. `frame` picks the part when that is built.
pub fn open_for(app: &mut App, r: &EditRequest) {
    if !r.document.is_empty() {
        let want = std::fs::canonicalize(&r.document).unwrap_or_else(|_| PathBuf::from(&r.document));
        let open = app.path.as_ref().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        if open.as_deref() != Some(want.as_path()) {
            if !want.is_file() {
                app.flash(format!("{} wasn't found", want.display()), ToastKind::Error);
                return;
            }
            if app.content_hash() != app.session.saved_hash {
                app.flash(format!("Save or close this document first to edit the part from {}", want.display()), ToastKind::Info);
                return;
            }
            app.open_path(want);
        }
    }
    let by_file = sent_part(app, r).map(|t| t.0);
    let Some(si) = by_file.or_else(|| app.ws.studios.iter().position(|s| s.name == r.studio)) else {
        app.flash(format!("The Part Studio \"{}\" isn't in this document", r.studio), ToastKind::Info);
        return;
    };
    if app.tab != crate::workspace::Tab::Studio(si) {
        app.switch_tab(crate::workspace::Tab::Studio(si));
    }
    app.orca.waiting = Some(r.clone());
}

/// The part a request names among the parts sent this session.
fn sent_part(app: &App, r: &EditRequest) -> Option<(usize, BodyId)> {
    if r.file.is_empty() {
        return None;
    }
    app.orca.sent.iter().find(|s| same_file(&s.file, Path::new(&r.file))).map(|s| (s.studio, s.body))
}

/// Select the part a request names in the open Part Studio.
fn pick(app: &mut App, r: &EditRequest) {
    let named = || {
        let named = app.doc.parts.iter().find(|(_, p)| p.name.as_deref() == Some(r.part.as_str())).and_then(|(k, _)| parse_body_key(k));
        named.or_else(|| app.result.bodies.iter().find(|b| b.name == r.part).map(|b| b.id))
    };
    let body = sent_part(app, r).filter(|t| t.0 == app.cur_studio).map(|t| t.1).or_else(named);
    match body.filter(|b| app.result.bodies.iter().any(|x| x.id == *b)) {
        Some(b) => {
            app.picks = vec![crate::doc::Ref::Body { body: b }];
            app.parts_open = true;
            app.scene_dirty = true;
        }
        None => app.flash(format!("The part \"{}\" is no longer in {}", r.part, r.studio), ToastKind::Info),
    }
}

/// The part named on the command line (`rcad doc.json --studio S --part P`, how the slicer
/// starts rCAD for "Edit in rCAD" when none is running).
pub fn request_from_args(args: &[String]) -> Option<EditRequest> {
    let doc = args.iter().skip(1).find(|a| !a.starts_with('-'))?.clone();
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let (studio, part) = (val("--studio")?, val("--part").unwrap_or_default());
    Some(EditRequest { document: doc, studio, part, file: String::new() })
}

fn parse_body_key(k: &str) -> Option<BodyId> {
    let (a, b) = k.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_requests_parse() {
        let r = parse_request_line("GET /edit?document=%2Fhome%2Fj%2Fbox.json&studio=Part+Studio+1&part=Lid%20%26%20hinge HTTP/1.1\r\n").unwrap();
        assert_eq!(r.document, "/home/j/box.json");
        assert_eq!(r.studio, "Part Studio 1");
        assert_eq!(r.part, "Lid & hinge");
        assert!(parse_request_line("GET /other HTTP/1.1").is_none());
        assert!(parse_request_line("POST /edit HTTP/1.1").is_none());
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%e2%9c%93"), "✓");
    }

    #[test]
    fn names_and_paths() {
        assert_eq!(safe_name("a/b: c?"), "a_b_ c_");
        assert_eq!(safe_name(" .. "), "part");
        assert_eq!(print_dir(Some(Path::new("/p/box.json"))), PathBuf::from("/p").join(DIR));
        assert_eq!(sidecar_path(Path::new("/p/a.step")), PathBuf::from("/p/a.step.rcad.json"));
        let args: Vec<String> = ["rcad", "/p/box.json", "--studio", "Lid", "--part", "Hinge"].iter().map(|s| s.to_string()).collect();
        assert_eq!(request_from_args(&args), Some(EditRequest { document: "/p/box.json".into(), studio: "Lid".into(), part: "Hinge".into(), file: String::new() }));
    }

    #[test]
    fn listener_takes_requests() {
        let mut l = Link::default();
        let addr = l.listen().unwrap();
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        write!(s, "GET /edit?document=d.json&studio=S&part=P&file=%2Ftmp%2Fx.step HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        std::io::Read::read_to_string(&mut s, &mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let got = l.take_requests();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].file, "/tmp/x.step");
    }
}
