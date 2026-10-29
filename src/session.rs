//! The editor session around the document: unsaved-changes tracking and the
//! "save changes?" prompt, autosave and recovery, a typed-path file prompt for
//! systems without a file dialog, remembered window settings,
//! undo for tab operations, and the confirmation windows that go with them.

use std::collections::HashSet;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Key, RichText};
use serde::{Deserialize, Serialize};

use crate::app::{App, ToastKind};
use crate::expr::Units;
use crate::theme;
use crate::units_switch::UnitChange;
use crate::workspace::{Tab, Workspace};

/// What to do once unsaved changes are dealt with.
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    Close,
    New,
    Open,
    OpenPath(PathBuf),
}

/// What a picked path is for.
pub type PathUse = Box<dyn FnOnce(&mut App, PathBuf)>;

/// The built-in file prompt: a path typed in, with the folder's files listed.
pub struct PathPrompt {
    title: String,
    text: String,
    exts: Vec<String>,
    save: bool,
    then: Option<PathUse>,
    focus: bool,
    error: Option<String>,
}

/// A workspace before a tab operation, for undoing it.
pub struct WsSnap {
    pub label: String,
    ws: Workspace,
    tab: Tab,
    cur_studio: usize,
    /// How the Part Studio list differs from the current one, so per-studio undo and
    /// geometry follow their studios: a studio this snapshot has at that index and the
    /// current workspace lacks (with its undo history), or one it lacks.
    restores_studio: Option<(usize, (Vec<crate::doc::Document>, Vec<crate::doc::Document>))>,
    drops_studio: Option<usize>,
    /// Content hash right after the operation: while the document still has it, Ctrl+Z undoes
    /// the operation (rather than an edit made after it).
    after: u64,
}

/// Settings kept between runs (UI zoom, window size). Recent files live in `home.rs`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Prefs {
    #[serde(default)]
    pub zoom: Option<f32>,
    #[serde(default)]
    pub window: Option<[f32; 2]>,
}

pub struct Session {
    /// Content hash at the last save or open, and the latest one.
    pub saved_hash: u64,
    pub cur_hash: u64,
    hashed_at: Option<Instant>,
    input_since_hash: bool,
    /// Something outside user input (Claude) changed the document.
    pub poke: bool,
    /// The "save changes?" prompt, and what runs after a save it started.
    pub confirm: Option<Pending>,
    after_save: Option<Pending>,
    allow_close: bool,
    /// The system has no file dialog: use the built-in prompt.
    pub no_native_dialog: bool,
    pub prompt: Option<PathPrompt>,
    autosaved: Option<(Instant, u64)>,
    /// Autosaves left by an earlier run that ended without saving.
    pub recover: Vec<PathBuf>,
    pub ws_undo: Vec<WsSnap>,
    pub ws_redo: Vec<WsSnap>,
    pub confirm_delete: Option<Tab>,
    pub unit_switch: Option<Units>,
    pub show_shortcuts: bool,
    pub prefs: Prefs,
    prefs_changed: Option<Instant>,
    /// Assembly tabs already framed once.
    pub fitted_asm: HashSet<usize>,
    /// Asked for where no egui context is at hand (command search): done next frame.
    pub toggle_dark: bool,
    pub screenshot: bool,
    /// Part Studio display: 0 shaded, 1 hidden line, 2 wireframe.
    pub render_mode: u8,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            saved_hash: 0,
            cur_hash: 0,
            hashed_at: None,
            input_since_hash: false,
            poke: false,
            confirm: None,
            after_save: None,
            allow_close: false,
            no_native_dialog: std::env::var_os("CAD_NO_FILE_DIALOG").is_some(),
            prompt: None,
            autosaved: None,
            recover: vec![],
            ws_undo: vec![],
            ws_redo: vec![],
            confirm_delete: None,
            unit_switch: None,
            show_shortcuts: false,
            prefs: load_prefs(),
            prefs_changed: None,
            fitted_asm: HashSet::new(),
            toggle_dark: false,
            screenshot: false,
            render_mode: 0,
        }
    }
}

// ---------------------------------------------------------------- storage

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cad-rust-app"))
}

pub fn load_prefs() -> Prefs {
    config_dir().and_then(|d| std::fs::read_to_string(d.join("ui.json")).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_prefs(p: &Prefs) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("ui.json"), serde_json::to_string_pretty(p).unwrap_or_default());
    }
}

fn autosave_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("autosave"))
}

/// This run's autosave name: process id and start time (process ids are reused).
fn run_stem() -> &'static str {
    static S: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        format!("{}-{t}", std::process::id())
    })
}

fn autosave_file() -> Option<PathBuf> {
    autosave_dir().map(|d| d.join(format!("{}.json", run_stem())))
}

/// Whether the run that wrote an autosave has ended.
fn writer_gone(file: &Path) -> bool {
    let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else { return true };
    if stem == run_stem() {
        return false;
    }
    let Some(pid) = stem.split('-').next().and_then(|s| s.parse::<u32>().ok()) else { return true };
    if pid == std::process::id() {
        // an earlier run that had our process id
        return true;
    }
    if cfg!(target_os = "linux") {
        return !Path::new("/proc").join(pid.to_string()).exists();
    }
    // elsewhere: a live run rewrites its file every minute while it has changes
    std::fs::metadata(file).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|e| e > Duration::from_secs(300)).unwrap_or(true)
}

/// Autosaves of earlier runs, newest first.
pub fn leftover_autosaves() -> Vec<PathBuf> {
    let Some(dir) = autosave_dir() else { return vec![] };
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false) && writer_gone(p))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|x| x.1).collect()
}

fn remove_autosave(file: &Path) {
    let _ = std::fs::remove_file(file);
    let _ = std::fs::remove_file(file.with_extension("path"));
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// Feeds serialized JSON straight into a hasher.
struct HashWriter(std::collections::hash_map::DefaultHasher);

impl std::io::Write for HashWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.write(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------- unsaved changes

impl App {
    /// A hash of everything that is saved (the live Part Studio included).
    pub fn content_hash(&self) -> u64 {
        let mut h = HashWriter(Default::default());
        for (i, s) in self.ws.studios.iter().enumerate() {
            h.0.write(s.name.as_bytes());
            let _ = serde_json::to_writer(&mut h, if i == self.cur_studio { &self.doc } else { &s.doc });
        }
        let ws = &self.ws;
        let _ = serde_json::to_writer(&mut h, &(&ws.assemblies, &ws.drawings, &ws.cams, &ws.sims, &ws.pcbs, &ws.tab_order));
        for v in &ws.versions {
            h.0.write(v.name.as_bytes());
            h.0.write(v.created.as_bytes());
        }
        h.0.finish()
    }

    pub fn is_dirty(&self) -> bool {
        self.session.cur_hash != self.session.saved_hash
    }

    /// The document as it is now counts as saved.
    pub fn mark_saved(&mut self) {
        let h = self.content_hash();
        self.session.saved_hash = h;
        self.session.cur_hash = h;
        self.session.hashed_at = Some(Instant::now());
        if let Some(f) = autosave_file() {
            remove_autosave(&f);
        }
        self.session.autosaved = None;
    }

    pub fn refresh_hash(&mut self) {
        self.session.cur_hash = self.content_hash();
        self.session.hashed_at = Some(Instant::now());
        self.session.input_since_hash = false;
        self.session.poke = false;
    }

    /// Once a frame: notice edits (rehashing at most a few times a second), autosave, remember
    /// window settings, and handle the window's close button.
    pub fn session_frame(&mut self, ctx: &egui::Context) {
        let edits = ctx.input(|i| {
            i.events.iter().any(|e| match e {
                egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::PointerButton { .. } => true,
                egui::Event::PointerMoved(_) => i.pointer.any_down(),
                _ => false,
            })
        });
        self.session.input_since_hash |= edits;
        if self.session.input_since_hash || self.session.poke {
            let due = self.session.hashed_at.map(|t| t.elapsed() >= Duration::from_millis(400)).unwrap_or(true);
            if due {
                self.refresh_hash();
            } else {
                ctx.request_repaint_after(Duration::from_millis(420));
            }
        }
        if std::mem::take(&mut self.session.toggle_dark) {
            self.set_dark(ctx, !self.dark);
        }
        if std::mem::take(&mut self.session.screenshot) {
            self.screenshot(ctx);
        }
        self.autosave();
        self.remember_window(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.session.allow_close {
            self.refresh_hash();
            if self.is_dirty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.session.confirm = Some(Pending::Close);
            } else if let Some(f) = autosave_file() {
                remove_autosave(&f);
            }
        }
    }

    /// Write unsaved work to the autosave folder once a minute.
    fn autosave(&mut self) {
        let Some(file) = autosave_file() else { return };
        let dirty = self.is_dirty();
        match self.session.autosaved {
            Some((t, h)) if t.elapsed() < Duration::from_secs(60) || (h == self.session.cur_hash && dirty) => return,
            None if !dirty => return,
            _ => {}
        }
        if !dirty {
            remove_autosave(&file);
            self.session.autosaved = None;
            return;
        }
        let mut ws = self.ws.clone();
        if let Some(s) = ws.studios.get_mut(self.cur_studio) {
            s.doc = self.doc.clone();
        }
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let ok = std::fs::write(&file, ws.to_json()).is_ok();
        let _ = std::fs::write(file.with_extension("path"), self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        if !ok {
            log::warn!("could not write the autosave {}", file.display());
        }
        self.session.autosaved = Some((Instant::now(), self.session.cur_hash));
    }

    fn remember_window(&mut self, ctx: &egui::Context) {
        let zoom = ctx.zoom_factor();
        // in ui points, which the zoom scales; the window builder wants logical pixels
        let size = ctx.input(|i| i.viewport().inner_rect.map(|r| [(r.width() * zoom).round(), (r.height() * zoom).round()]));
        let p = &mut self.session.prefs;
        let mut changed = false;
        if p.zoom != Some(zoom) {
            p.zoom = Some(zoom);
            changed = true;
        }
        if size.is_some() && p.window != size && !ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
            p.window = size;
            changed = true;
        }
        if changed {
            self.session.prefs_changed = Some(Instant::now());
        }
        // write once things settle (a window drag resizes every frame)
        if let Some(t) = self.session.prefs_changed {
            if t.elapsed() > Duration::from_secs(1) {
                save_prefs(&self.session.prefs);
                self.session.prefs_changed = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(1100));
            }
        }
    }

    /// Run `p` now, or first ask about unsaved changes.
    pub fn guard(&mut self, p: Pending) {
        self.refresh_hash();
        if self.is_dirty() {
            self.session.confirm = Some(p);
        } else {
            self.run_pending(p);
        }
    }

    pub fn run_pending(&mut self, p: Pending) {
        match p {
            Pending::Close => {
                if let Some(f) = autosave_file() {
                    remove_autosave(&f);
                }
                // the frame loop sends ViewportCommand::Close
                self.session.allow_close = true;
                self.session.prompt = None;
                self.session.confirm = None;
            }
            Pending::New => {
                self.new_document();
                self.home.open = false;
            }
            Pending::Open => self.with_open_path("Open", "Part", &["json"], |a, p| a.open_from_home(p)),
            Pending::OpenPath(p) => self.open_from_home(p),
        }
    }

    /// Open `p`, leaving the start screen unless the file could not be read.
    fn open_from_home(&mut self, p: PathBuf) {
        self.open_path(p.clone());
        if self.path.as_deref() == Some(p.as_path()) {
            self.home.open = false;
        }
    }

    /// Called after a save: carries on with what the save was for.
    pub fn saved_ok(&mut self, p: &Path) {
        self.mark_saved();
        crate::home::touch(self, p);
        if let Some(next) = self.session.after_save.take() {
            self.run_pending(next);
        }
    }

    // ------------------------------------------------------------ file prompts

    /// Ask for a file to open, then run `then` with it. Without a system file dialog the built-in
    /// prompt asks instead and `then` runs when it is accepted.
    pub fn with_open_path(&mut self, title: &str, filter: &str, exts: &[&str], then: impl FnOnce(&mut App, PathBuf) + 'static) {
        if !self.session.no_native_dialog {
            let t = Instant::now();
            let picked = rfd::FileDialog::new().add_filter(filter, exts).pick_file();
            if let Some(p) = picked {
                return then(self, p);
            }
            if t.elapsed() > Duration::from_millis(300) {
                return; // the user cancelled the dialog
            }
            log::warn!("no file dialog available; using the built-in prompt");
            self.session.no_native_dialog = true;
        }
        let dir = self.start_dir();
        self.session.prompt = Some(PathPrompt {
            title: title.to_string(),
            text: format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR),
            exts: exts.iter().map(|e| e.to_lowercase()).collect(),
            save: false,
            then: Some(Box::new(then)),
            focus: true,
            error: None,
        });
    }

    /// Ask where to save `name`, then run `then` with the path.
    pub fn with_save_path(&mut self, title: &str, filter: &str, exts: &[&str], name: &str, then: impl FnOnce(&mut App, PathBuf) + 'static) {
        if !self.session.no_native_dialog {
            let t = Instant::now();
            let mut d = rfd::FileDialog::new().add_filter(filter, exts).set_file_name(name);
            if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()).filter(|d| d.is_dir()) {
                d = d.set_directory(dir);
            }
            if let Some(p) = d.save_file() {
                return then(self, p);
            }
            if t.elapsed() > Duration::from_millis(300) {
                return;
            }
            log::warn!("no file dialog available; using the built-in prompt");
            self.session.no_native_dialog = true;
        }
        let dir = self.start_dir();
        self.session.prompt = Some(PathPrompt {
            title: title.to_string(),
            text: dir.join(name).display().to_string(),
            exts: exts.iter().map(|e| e.to_lowercase()).collect(),
            save: true,
            then: Some(Box::new(then)),
            focus: true,
            error: None,
        });
    }

    /// The document's folder, else the home folder.
    fn start_dir(&self) -> PathBuf {
        let dir = self.path.as_ref().and_then(|p| std::path::absolute(p).ok()).and_then(|p| p.parent().map(|d| d.to_path_buf())).filter(|d| d.is_dir());
        dir.or_else(|| std::env::var_os("HOME").map(PathBuf::from)).unwrap_or_else(|| ".".into())
    }

    // ------------------------------------------------------------ tab undo

    fn snap(&self, label: String) -> WsSnap {
        let mut ws = self.ws.clone();
        if let Some(s) = ws.studios.get_mut(self.cur_studio) {
            s.doc = self.doc.clone();
        }
        WsSnap { label, ws, tab: self.tab, cur_studio: self.cur_studio, restores_studio: None, drops_studio: None, after: 0 }
    }

    /// Remember the workspace before a tab operation. `removing_studio`/`adding_studio` say how the
    /// operation changes the Part Studio list.
    pub fn ws_checkpoint(&mut self, label: impl Into<String>, removing_studio: Option<usize>, adding_studio: bool) {
        let mut s = self.snap(label.into());
        if let Some(i) = removing_studio {
            let hist = if i == self.cur_studio { (self.undo.clone(), self.redo.clone()) } else { self.studio_undo.get(&i).cloned().unwrap_or_default() };
            s.restores_studio = Some((i, hist));
        }
        if adding_studio {
            s.drops_studio = Some(self.ws.studios.len());
        }
        self.session.ws_undo.push(s);
        if self.session.ws_undo.len() > 30 {
            self.session.ws_undo.remove(0);
        }
        self.session.ws_redo.clear();
    }

    /// Call after the tab operation is done: until something else changes, Ctrl+Z undoes it.
    pub fn ws_mark(&mut self) {
        self.refresh_hash();
        if let Some(s) = self.session.ws_undo.last_mut() {
            s.after = self.session.cur_hash;
        }
    }

    /// The last change was a tab operation (or undoing one), so undo (redo) is at workspace level.
    pub fn ws_latest(&self, redo: bool) -> bool {
        let st = if redo { &self.session.ws_redo } else { &self.session.ws_undo };
        st.last().map(|s| s.after == self.session.cur_hash).unwrap_or(false)
    }

    pub fn ws_undo_step(&mut self, redo: bool) -> bool {
        let snap = if redo { self.session.ws_redo.pop() } else { self.session.ws_undo.pop() };
        let Some(snap) = snap else { return false };
        let label = snap.label.clone();
        let mut back = self.apply_snap(snap);
        self.refresh_hash();
        back.after = self.session.cur_hash;
        if redo {
            self.session.ws_undo.push(back);
        } else {
            self.session.ws_redo.push(back);
        }
        self.flash_info(format!("{} {label}", if redo { "Redid" } else { "Undid" }), Some(Duration::from_secs(2)));
        true
    }

    /// Put a snapshot's workspace back, keeping each Part Studio's undo history with it, and
    /// return the snapshot that goes the other way.
    fn apply_snap(&mut self, s: WsSnap) -> WsSnap {
        if self.sketch.is_some() {
            crate::sketcher::finish(self);
        }
        self.dialog = None;
        self.sync_ws();
        let mut back = self.snap(s.label.clone());
        // park the live studio's history with the others
        let cur = self.cur_studio;
        self.studio_undo.insert(cur, (std::mem::take(&mut self.undo), std::mem::take(&mut self.redo)));
        if let Some((i, hist)) = s.restores_studio {
            // the snapshot has studio i, the current workspace doesn't: move later ones up
            let up = |k: usize| if k >= i { k + 1 } else { k };
            self.studio_undo = std::mem::take(&mut self.studio_undo).into_iter().map(|(k, v)| (up(k), v)).collect();
            self.studio_undo.insert(i, hist);
            back.drops_studio = Some(i);
        }
        if let Some(i) = s.drops_studio {
            let hist = self.studio_undo.remove(&i).unwrap_or_default();
            let down = |k: usize| if k > i { k - 1 } else { k };
            self.studio_undo = std::mem::take(&mut self.studio_undo).into_iter().map(|(k, v)| (down(k), v)).collect();
            back.restores_studio = Some((i, hist));
        }
        let n = s.ws.studios.len();
        self.ws = s.ws;
        self.studio_undo.retain(|k, _| *k < n);
        self.cur_studio = s.cur_studio.min(n - 1);
        let (u, r) = self.studio_undo.remove(&self.cur_studio).unwrap_or_default();
        self.undo = u;
        self.redo = r;
        self.doc = self.ws.studios[self.cur_studio].doc.clone();
        // geometry of every studio is rebuilt (indices and content may have moved); the old
        // meshes stay on screen until then
        self.studio_results.clear();
        self.picks.clear();
        self.asm.sel.clear();
        self.asm.sel_mate = None;
        self.asm.dialog = None;
        self.cam = Default::default();
        self.fea = Default::default();
        self.pcb = Default::default();
        self.renaming_tab = None;
        self.changed();
        let t = s.tab;
        let valid = match t {
            Tab::Studio(i) => i < self.ws.studios.len(),
            Tab::Assembly(i) => i < self.ws.assemblies.len(),
            Tab::Drawing(i) => i < self.ws.drawings.len(),
            Tab::Cam(i) => i < self.ws.cams.len(),
            Tab::Sim(i) => i < self.ws.sims.len(),
            Tab::Pcb(i) => i < self.ws.pcbs.len(),
        };
        self.tab = Tab::Studio(self.cur_studio);
        self.switch_tab(if valid { t } else { Tab::Studio(self.cur_studio) });
        back
    }
}

// ---------------------------------------------------------------- windows

/// Every session window: the save prompt, file prompt, recovery offer, tab delete and units
/// confirmations, and the shortcut list.
pub fn windows(app: &mut App, ctx: &egui::Context) {
    if app.session.allow_close {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if let Some(p) = app.session.confirm.clone() {
        save_prompt(app, ctx, p);
    }
    if app.session.prompt.is_some() {
        path_prompt(app, ctx);
    }
    if !app.session.recover.is_empty() && app.session.confirm.is_none() {
        recover_prompt(app, ctx);
    }
    if let Some(t) = app.session.confirm_delete {
        delete_prompt(app, ctx, t);
    }
    if let Some(u) = app.session.unit_switch {
        units_prompt(app, ctx, u);
    }
    if app.session.show_shortcuts {
        shortcuts_window(app, ctx);
    }
}

fn modal_frame(ui: &mut egui::Ui, title: &str) {
    ui.set_max_width(460.0);
    ui.label(RichText::new(title).family(theme::medium()).size(15.0));
    ui.add_space(6.0);
}

fn doc_name(app: &App) -> String {
    app.path.as_ref().map(|p| file_name(p)).unwrap_or_else(|| "Untitled document".into())
}

fn save_prompt(app: &mut App, ctx: &egui::Context, p: Pending) {
    let what = match p {
        Pending::Close => "closing",
        Pending::New => "starting a new document",
        Pending::Open | Pending::OpenPath(_) => "opening another document",
    };
    let mut choice = None;
    egui::Modal::new(egui::Id::new("save-changes")).show(ctx, |ui| {
        modal_frame(ui, &format!("Save changes to “{}”?", doc_name(app)));
        ui.label(RichText::new(format!("Your changes will be lost if you don't save them before {what}.")).color(theme::muted()));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Save").color(egui::Color32::WHITE)).fill(theme::accent())).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                choice = Some(0);
            }
            if ui.button("Don't save").clicked() {
                choice = Some(1);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                choice = Some(2);
            }
        });
    });
    match choice {
        Some(0) => {
            app.session.confirm = None;
            app.session.after_save = Some(p);
            app.save(false);
        }
        Some(1) => {
            app.session.confirm = None;
            app.run_pending(p);
        }
        Some(_) => {
            app.session.confirm = None;
            app.session.after_save = None;
        }
        None => {}
    }
}

fn path_prompt(app: &mut App, ctx: &egui::Context) {
    let Some(pr) = app.session.prompt.as_mut() else { return };
    let mut accept = false;
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("path-prompt")).show(ctx, |ui| {
        ui.set_width(520.0f32.min(ctx.content_rect().width() - 40.0));
        modal_frame(ui, &pr.title);
        ui.label(RichText::new("No system file dialog is available, so type the file's path.").color(theme::muted()).size(12.0));
        ui.add_space(4.0);
        let r = ui.add(egui::TextEdit::singleline(&mut pr.text).desired_width(f32::INFINITY).margin(egui::vec2(8.0, 6.0)));
        if pr.focus {
            r.request_focus();
            pr.focus = false;
        }
        if r.changed() {
            pr.error = None;
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            accept = true;
        }
        // the folder's contents: folders to go into and files of the right type
        let typed = PathBuf::from(pr.text.trim());
        let dir = if pr.text.ends_with(std::path::MAIN_SEPARATOR) || typed.is_dir() { typed.clone() } else { typed.parent().map(|d| d.to_path_buf()).unwrap_or_default() };
        let mut entries: Vec<(bool, String)> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = e.path().is_dir();
                        let ext_ok = e.path().extension().map(|x| pr.exts.contains(&x.to_string_lossy().to_lowercase())).unwrap_or(false);
                        (!name.starts_with('.') && (is_dir || ext_ok)).then_some((is_dir, name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        ui.add_space(4.0);
        egui::Frame::new().fill(theme::panel_2()).corner_radius(6).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| {
                if let Some(up) = dir.parent() {
                    if ui.add(egui::Button::image_and_text(crate::icons::image("folder", 14.0).tint(theme::muted()), "..").frame(false)).clicked() {
                        pr.text = format!("{}{}", up.display(), std::path::MAIN_SEPARATOR);
                        pr.focus = true;
                    }
                }
                for (is_dir, name) in &entries {
                    let b = if *is_dir {
                        ui.add(egui::Button::image_and_text(crate::icons::image("folder", 14.0).tint(theme::muted()), name.as_str()).frame(false))
                    } else {
                        ui.add(egui::Button::new(name.as_str()).frame(false))
                    };
                    if b.clicked() {
                        let p = dir.join(name);
                        if *is_dir {
                            pr.text = format!("{}{}", p.display(), std::path::MAIN_SEPARATOR);
                        } else {
                            pr.text = p.display().to_string();
                        }
                        pr.focus = true;
                    }
                    if b.double_clicked() && !is_dir {
                        accept = true;
                    }
                }
                if entries.is_empty() {
                    ui.label(RichText::new("No matching files here").color(theme::muted()).size(12.0));
                }
            });
        });
        if let Some(e) = &pr.error {
            ui.label(RichText::new(e).color(theme::error()));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if pr.save { "Save" } else { "Open" };
            if ui.add(egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(theme::accent())).clicked() {
                accept = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                cancel = true;
            }
        });
    });
    if cancel {
        app.session.prompt = None;
        app.session.after_save = None;
        return;
    }
    if !accept {
        return;
    }
    let pr = app.session.prompt.as_mut().unwrap();
    let mut p = PathBuf::from(pr.text.trim());
    if let Some(rest) = pr.text.trim().strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            p = PathBuf::from(h).join(rest);
        }
    }
    // saving without an extension gets the first one
    if pr.save && p.extension().is_none() {
        if let Some(e) = pr.exts.first() {
            p.set_extension(e);
        }
    }
    let err = if p.as_os_str().is_empty() || p.is_dir() {
        Some("Type a file name".to_string())
    } else if !pr.save && !p.is_file() {
        Some(format!("There is no file {}", p.display()))
    } else if pr.save && !p.parent().map(|d| d.as_os_str().is_empty() || d.is_dir()).unwrap_or(true) {
        Some(format!("There is no folder {}", p.parent().unwrap().display()))
    } else {
        None
    };
    if let Some(e) = err {
        pr.error = Some(e);
        return;
    }
    let then = pr.then.take();
    app.session.prompt = None;
    if let Some(f) = then {
        f(app, p);
    }
}

fn recover_prompt(app: &mut App, ctx: &egui::Context) {
    let file = app.session.recover[0].clone();
    let orig = std::fs::read_to_string(file.with_extension("path")).ok().filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    let when = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|e| match e.as_secs() {
            s if s < 120 => "a minute ago".to_string(),
            s if s < 7200 => format!("{} minutes ago", s / 60),
            s if s < 172800 => format!("{} hours ago", s / 3600),
            s => format!("{} days ago", s / 86400),
        })
        .unwrap_or_default();
    let mut choice = None;
    egui::Modal::new(egui::Id::new("recover")).show(ctx, |ui| {
        modal_frame(ui, "Recover unsaved work?");
        let name = orig.as_ref().map(|p| file_name(p)).unwrap_or_else(|| "an untitled document".into());
        ui.label(format!("cad-app closed without saving changes to {name}. An autosave from {when} is available."));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Recover").color(egui::Color32::WHITE)).fill(theme::accent())).clicked() {
                choice = Some(0);
            }
            if ui.button("Discard").on_hover_text("Delete the autosave").clicked() {
                choice = Some(1);
            }
            if ui.button("Not now").on_hover_text("Keep the autosave for next time").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                choice = Some(2);
            }
        });
    });
    match choice {
        Some(0) => {
            app.session.recover.clear();
            match std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|s| Workspace::from_json(&s)) {
                Ok(ws) => {
                    app.load_workspace(ws);
                    app.path = orig;
                    // unsaved until the user saves it
                    app.refresh_hash();
                    app.session.saved_hash = app.session.cur_hash.wrapping_add(1);
                    remove_autosave(&file);
                    app.home.open = false;
                    app.flash_info("Recovered the unsaved work · save it to keep it", Some(Duration::from_secs(4)));
                }
                Err(e) => app.flash(format!("Could not recover: {e}"), ToastKind::Error),
            }
        }
        Some(1) => {
            remove_autosave(&file);
            app.session.recover.remove(0);
        }
        Some(_) => app.session.recover.clear(),
        None => {}
    }
}

/// What deleting tab `t` takes with it, one line each.
pub fn delete_effects(app: &App, t: Tab) -> Vec<String> {
    let ws = &app.ws;
    let mut out = vec![];
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match t {
        Tab::Studio(i) => {
            for a in &ws.assemblies {
                let gone: HashSet<u64> = a.instances.iter().filter(|x| x.sub.is_none() && x.studio == i).map(|x| x.id).collect();
                if !gone.is_empty() {
                    let mates = a.mates.iter().filter(|m| [&m.a, &m.b].iter().any(|c| c.map(|c| gone.contains(&c.inst)).unwrap_or(false))).count();
                    let mut l = format!("{}: {} removed", a.name, plural(gone.len(), "instance", "instances"));
                    if mates > 0 {
                        l += &format!(", {} with them", plural(mates, "mate", "mates"));
                    }
                    out.push(l);
                }
            }
            for d in &ws.drawings {
                let n = d.views.iter().filter(|v| v.studio == i).count();
                if n > 0 {
                    out.push(format!("{}: {} removed", d.name, plural(n, "view", "views")));
                }
            }
            for c in ws.cams.iter().filter(|c| c.studio == i) {
                out.push(format!("{}: loses its part selection", c.name));
            }
            for s in ws.sims.iter().filter(|s| s.studio == i) {
                out.push(format!("{}: loses its parts, supports and loads", s.name));
            }
            for (k, s) in ws.studios.iter().enumerate() {
                let n = s.doc.features.iter().filter(|f| matches!(f.op, crate::doc::Op::Derived { studio, .. } if studio == i)).count();
                if k != i && n > 0 {
                    out.push(format!("{}: {} will fail", s.name, plural(n, "Derived feature", "Derived features")));
                }
            }
        }
        Tab::Assembly(i) => {
            for a in &ws.assemblies {
                let gone: HashSet<u64> = a.instances.iter().filter(|x| x.sub == Some(i)).map(|x| x.id).collect();
                if !gone.is_empty() {
                    let mates = a.mates.iter().filter(|m| [&m.a, &m.b].iter().any(|c| c.map(|c| gone.contains(&c.inst)).unwrap_or(false))).count();
                    let mut l = format!("{}: {} of it removed", a.name, plural(gone.len(), "instance", "instances"));
                    if mates > 0 {
                        l += &format!(", {} with them", plural(mates, "mate", "mates"));
                    }
                    out.push(l);
                }
            }
        }
        Tab::Cam(i) => {
            let name = &ws.cams[i].name;
            for c in ws.cams.iter().filter(|c| c.follows == *name) {
                out.push(format!("{}: no longer continues from it", c.name));
            }
        }
        Tab::Drawing(_) | Tab::Sim(_) | Tab::Pcb(_) => {}
    }
    out
}

/// Drop mates, relations and explode steps that point at instances no longer there.
pub fn prune_orphans(a: &mut crate::assembly::Assembly) -> usize {
    let ids: HashSet<u64> = a.instances.iter().map(|x| x.id).collect();
    let n = a.mates.len();
    a.mates.retain(|m| [&m.a, &m.b].iter().all(|c| c.map(|c| ids.contains(&c.inst)).unwrap_or(true)));
    let mates: HashSet<u64> = a.mates.iter().map(|m| m.id).collect();
    a.relations.retain(|r| mates.contains(&r.a) && r.b.map(|b| mates.contains(&b)).unwrap_or(true));
    for s in &mut a.explode_steps {
        s.insts.retain(|i| ids.contains(i));
    }
    a.explode_steps.retain(|s| !s.insts.is_empty());
    n - a.mates.len()
}

fn tab_name(app: &App, t: Tab) -> String {
    let ws = &app.ws;
    match t {
        Tab::Studio(i) => ws.studios.get(i).map(|s| s.name.clone()),
        Tab::Assembly(i) => ws.assemblies.get(i).map(|s| s.name.clone()),
        Tab::Drawing(i) => ws.drawings.get(i).map(|s| s.name.clone()),
        Tab::Cam(i) => ws.cams.get(i).map(|s| s.name.clone()),
        Tab::Sim(i) => ws.sims.get(i).map(|s| s.name.clone()),
        Tab::Pcb(i) => ws.pcbs.get(i).map(|s| s.name.clone()),
    }
    .unwrap_or_default()
}

fn delete_prompt(app: &mut App, ctx: &egui::Context, t: Tab) {
    let effects = delete_effects(app, t);
    let name = tab_name(app, t);
    let mut choice = None;
    egui::Modal::new(egui::Id::new("delete-tab")).show(ctx, |ui| {
        modal_frame(ui, &format!("Delete “{name}”?"));
        if effects.is_empty() {
            ui.label(RichText::new("Nothing else uses this tab.").color(theme::muted()));
        } else {
            ui.label("This also changes other tabs:");
            for e in &effects {
                ui.label(format!("  • {e}"));
            }
        }
        ui.label(RichText::new("Ctrl+Z brings it back.").color(theme::muted()).size(12.0));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Delete").color(egui::Color32::WHITE)).fill(theme::error())).clicked() {
                choice = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                choice = Some(false);
            }
        });
    });
    if let Some(yes) = choice {
        app.session.confirm_delete = None;
        if yes {
            app.tab_action("delete-confirmed", t);
        }
    }
}

fn units_prompt(app: &mut App, ctx: &egui::Context, to: Units) {
    let from = app.doc.units;
    let mut choice = None;
    egui::Modal::new(egui::Id::new("units-switch")).show(ctx, |ui| {
        modal_frame(ui, &format!("Switch this Part Studio from {} to {}?", from.label(), to.label()));
        ui.label(RichText::new("Plain numbers are read in the document's units. Choose what happens to the ones already typed:").color(theme::muted()));
        ui.add_space(8.0);
        let ex = crate::units_switch::rewrite("8", &Default::default(), from, to, UnitChange::Convert).unwrap_or_default();
        let opts = [
            (UnitChange::Convert, format!("Convert values (8 → {ex})"), "Rewrite plain numbers in the new unit. The model keeps its size."),
            (
                UnitChange::Label,
                format!("Keep numbers, add “{}” (8 → 8 {})", from.label(), from.label()),
                "Plain numbers get the old unit written after them. The model keeps its size.",
            ),
            (UnitChange::Rescale, format!("Rescale the model (8 → 8 {})", to.label()), "Read the same numbers in the new unit: the model changes size."),
        ];
        for (k, (how, label, tip)) in opts.iter().enumerate() {
            let mut b =
                egui::Button::new(if k == 0 { RichText::new(label).color(egui::Color32::WHITE) } else { RichText::new(label) }).min_size(egui::vec2(ui.available_width(), 28.0));
            if k == 0 {
                b = b.fill(theme::accent());
            }
            if ui.add(b).on_hover_text(*tip).clicked() {
                choice = Some(Some(*how));
            }
        }
        ui.add_space(4.0);
        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
            choice = Some(None);
        }
    });
    if let Some(c) = choice {
        app.session.unit_switch = None;
        if let Some(how) = c {
            app.set_units(to, how);
        }
    }
}

impl App {
    /// Change the live Part Studio's units.
    pub fn set_units(&mut self, to: Units, how: UnitChange) {
        if to == self.doc.units {
            return;
        }
        self.checkpoint();
        let n = crate::units_switch::switch_units(&mut self.doc, to, how);
        // rescaling moves sketch geometry to the dimensions' new sizes
        self.solve_sketches();
        self.changed();
        self.scene_dirty = true;
        if n > 0 && how != UnitChange::Rescale {
            self.flash_info(format!("Units are now {} · {n} values rewritten to keep their size", to.label()), Some(Duration::from_secs(3)));
        }
    }

    /// The units box changed: ask how to treat existing numbers when any depend on the units.
    pub fn request_units(&mut self, to: Units) {
        let mut probe = self.doc.clone();
        if crate::units_switch::switch_units(&mut probe, to, UnitChange::Convert) == 0 {
            self.set_units(to, UnitChange::Rescale);
        } else {
            self.session.unit_switch = Some(to);
        }
    }
}

/// Keyboard shortcuts by area, for the in-app list.
pub const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Everywhere",
        &[
            ("Ctrl+Z / Ctrl+Shift+Z, Ctrl+Y", "Undo / redo"),
            ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
            ("Ctrl+O, Ctrl+N", "Open, new document"),
            ("S, / or Ctrl+K", "Search commands"),
            ("Ctrl+L", "Design with Claude"),
            ("F1 or ?", "This list"),
            ("Ctrl + / Ctrl − / Ctrl 0", "Zoom the interface"),
        ],
    ),
    (
        "Part Studio",
        &[
            ("Shift+S, E, R, F, H", "Sketch, extrude, revolve, fillet, hole"),
            ("0 1 2 3", "Iso, front, top, right view"),
            ("F", "Zoom to fit"),
            ("N", "Look normal to the selection"),
            ("M", "Measure"),
            ("Ctrl+A", "Select every part"),
            ("Enter / Esc", "Accept / cancel a feature; Esc clears the selection"),
        ],
    ),
    (
        "Sketch",
        &[
            ("L, R, C, A, …", "Drawing tools (hover a tool for its key)"),
            ("Ctrl+A", "Select everything"),
            ("Q", "Construction on/off"),
            ("G", "Grid snap on/off"),
            ("D", "Dimension"),
            ("Enter", "Finish the sketch"),
            ("Shift+E", "Finish and extrude"),
            ("Del", "Delete the selection"),
        ],
    ),
    ("Assembly", &[("Ctrl+A", "Select every instance"), ("Del", "Delete the selection"), ("F, 0–3", "Fit, standard views")]),
    ("Drawing", &[("S, D, R, A, N", "Select, dimension, radial, angle, note"), ("F", "Fit the sheet")]),
    ("Manufacture, Simulation", &[("Space", "Play the simulation / animation"), ("F", "Zoom to fit")]),
    ("Viewport mouse", &[("Right drag", "Orbit"), ("Middle drag or Shift+right drag", "Pan"), ("Wheel", "Zoom at the pointer"), ("Shift/Ctrl+click", "Add to the selection")]),
];

fn shortcuts_window(app: &mut App, ctx: &egui::Context) {
    let mut open = true;
    egui::Window::new("Keyboard shortcuts")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .default_width(460.0)
        .default_height(520.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (group, keys) in SHORTCUTS {
                    ui.label(RichText::new(*group).family(theme::medium()).size(13.5));
                    egui::Grid::new(("keys", *group)).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                        for (k, what) in *keys {
                            ui.label(RichText::new(*k).monospace().size(12.0));
                            ui.label(RichText::new(*what).color(theme::muted()).size(12.5));
                            ui.end_row();
                        }
                    });
                    ui.add_space(8.0);
                }
            });
        });
    if !open || ctx.input(|i| i.key_pressed(Key::Escape)) {
        app.session.show_shortcuts = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_mates_are_pruned() {
        use crate::assembly::{Assembly, Connector, Instance, Mate, MateKind};
        let inst = |id| Instance { id, name: String::new(), studio: 0, body: (0, 0).into(), pose: Default::default(), fixed: false, hidden: false, sub: None };
        let con = |inst| Some(Connector { inst, origin: [0.0; 3], z: [0.0, 0.0, 1.0], x: [1.0, 0.0, 0.0], on: None });
        let mate = |id, a, b| Mate {
            id,
            name: String::new(),
            kind: MateKind::Fastened,
            a: con(a),
            b: con(b),
            flip: false,
            offset: 0.0,
            offset_xy: [0.0; 2],
            rotation: 0.0,
            limits: None,
            suppressed: false,
        };
        let mut a = Assembly { instances: vec![inst(1), inst(2)], mates: vec![mate(10, 1, 2), mate(11, 1, 3)], ..Default::default() };
        assert_eq!(prune_orphans(&mut a), 1);
        assert_eq!(a.mates.len(), 1);
        assert_eq!(a.mates[0].id, 10);
    }

    #[test]
    fn prefs_round_trip() {
        let p = Prefs { zoom: Some(1.25), window: Some([1200.0, 800.0]) };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Prefs>(&s).unwrap(), p);
        assert_eq!(serde_json::from_str::<Prefs>("{}").unwrap(), Prefs::default());
    }
}
