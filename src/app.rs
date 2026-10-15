//! Application state and the window layout: toolbar, feature tree and variables
//! sidebar, viewport, status bar — the same arrangement as cad-app.html.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Color32, CornerRadius, Frame, Key, Layout, Margin, RichText, Sense, Stroke, Ui};

use crate::camera::{Camera, StdView};
use crate::doc::{self, Document, FeatId, Feature, Op, OriginPlane, Ref};
use crate::expr::Units;
use crate::icons;
use crate::kernel::RebuildResult;
use crate::render::SceneData;
use crate::sketcher::SketchSession;
use crate::theme;
use crate::worker::{KernelClient, KernelState, Outcome};

/// Feature buttons in toolbar groups, as in the original (`Am`).
pub const FEATURE_GROUPS: &[&[&str]] = &[
    &["sketch", "plane"],
    &["extrude", "revolve", "sweep", "loft"],
    &["fillet", "chamfer", "shell", "hole", "draft"],
    &["linearPattern", "circularPattern", "mirror", "boolean", "split"],
    &["thicken", "rib", "helix", "transform", "moveFace", "deleteFace"],
    &["sheetMetal"],
];
/// Buttons whose label is shown on the desktop toolbar (`jm`).
pub const PRIMARY: &[&str] = &["sketch"];

pub fn shortcut(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "sketch" => "Shift+S",
        "extrude" => "Shift+E",
        "revolve" => "Shift+R",
        "fillet" => "Shift+F",
        "hole" => "Shift+H",
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub until: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    /// 0 = XY (top), 1 = XZ (front), 2 = YZ (right)
    pub plane: u8,
    pub flip: bool,
    pub offset: f64,
}

/// The feature dialog: a feature inserted into the document that is being edited.
pub struct Dialog {
    pub feature: FeatId,
    pub is_new: bool,
    /// Document before the dialog opened, restored on cancel.
    pub before: Document,
    /// Which selection field viewport clicks go to.
    pub active_field: usize,
}

#[derive(Default)]
pub struct Measure {
    pub picks: Vec<Ref>,
    /// Kept measurements shown on the model: (a, b) world points and label.
    pub kept: Vec<([f64; 3], [f64; 3], String)>,
}

pub struct App {
    pub doc: Document,
    pub undo: Vec<Document>,
    pub redo: Vec<Document>,
    pub kernel: KernelClient,
    pub result: RebuildResult,
    pub errors: HashMap<FeatId, String>,
    /// Document changed since the last rebuild request.
    pub dirty_model: bool,
    pub scene: Arc<SceneData>,
    pub scene_dirty: bool,
    pub camera: Camera,
    pub picks: Vec<Ref>,
    pub hover: Option<Ref>,
    pub sketch: Option<SketchSession>,
    pub dialog: Option<Dialog>,
    pub show_dims: bool,
    pub xray: bool,
    pub measure: Option<Measure>,
    pub section: Option<Section>,
    pub sidebar_open: bool,
    pub phone: bool,
    pub toast: Option<Toast>,
    pub status: String,
    pub var_name: String,
    pub var_value: String,
    pub renaming: Option<(FeatId, String)>,
    pub renaming_part: Option<(String, String)>,
    pub editing_var: Option<(usize, String)>,
    pub path: Option<std::path::PathBuf>,
    pub viewport_rect: egui::Rect,
    pub nav: Option<crate::viewport::Nav>,
    pub first_fit: bool,
    pub ws: crate::workspace::Workspace,
    pub tab: crate::workspace::Tab,
    pub cur_studio: usize,
    pub studio_results: HashMap<usize, RebuildResult>,
    pub studio_undo: HashMap<usize, (Vec<Document>, Vec<Document>)>,
    pub asm: crate::asm_ui::AsmUi,
    pub dwg: crate::drawing_ui::DrawUi,
    pub renaming_tab: Option<(crate::workspace::Tab, String)>,
    pub version_name: String,
    pub restore_version: Option<usize>,
    pub screenshot_pending: Option<std::path::PathBuf>,
    pub context_menu: Option<(egui::Pos2, Option<Ref>)>,
    pub left_pane: LeftPane,
    pub tree_filter: String,
    pub geom_open: bool,
    pub parts_open: bool,
    pub tool_search: String,
    pub dark: bool,
}

/// What the left panel shows; picked with the icon rail on the far left.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LeftPane {
    Features,
    Variables,
    Configurations,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let dark = load_dark_pref();
        theme::set_dark(&cc.egui_ctx, dark);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        if let Some(rs) = &cc.wgpu_render_state {
            crate::render::Renderer::install(rs);
        }
        let mut app = App {
            doc: Document::default(),
            undo: vec![],
            redo: vec![],
            kernel: KernelClient::new(cc.egui_ctx.clone()),
            result: RebuildResult::default(),
            errors: HashMap::new(),
            dirty_model: true,
            scene: Arc::new(SceneData::default()),
            scene_dirty: true,
            camera: Camera::default(),
            picks: vec![],
            hover: None,
            sketch: None,
            dialog: None,
            show_dims: true,
            xray: false,
            measure: None,
            section: None,
            sidebar_open: true,
            phone: false,
            toast: None,
            status: String::new(),
            var_name: String::new(),
            var_value: String::new(),
            renaming: None,
            renaming_part: None,
            editing_var: None,
            path: None,
            viewport_rect: egui::Rect::NOTHING,
            nav: None,
            first_fit: true,
            ws: crate::workspace::Workspace::default(),
            tab: crate::workspace::Tab::Studio(0),
            cur_studio: 0,
            studio_results: HashMap::new(),
            studio_undo: HashMap::new(),
            asm: Default::default(),
            dwg: Default::default(),
            renaming_tab: None,
            version_name: String::new(),
            restore_version: None,
            screenshot_pending: None,
            context_menu: None,
            left_pane: LeftPane::Features,
            tree_filter: String::new(),
            geom_open: true,
            parts_open: true,
            tool_search: String::new(),
            dark,
        };
        if let Some(p) = std::env::args().skip(1).find(|a| !a.starts_with("--")) {
            app.open_path(std::path::PathBuf::from(p));
        }
        app.flash_info("Loading the OpenCascade kernel…", None);
        app
    }

    // ------------------------------------------------------------ messages

    pub fn flash(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toast = Some(Toast { text: text.into(), kind, until: Some(Instant::now() + Duration::from_secs(4)) });
    }

    pub fn flash_info(&mut self, text: impl Into<String>, dur: Option<Duration>) {
        self.toast = Some(Toast { text: text.into(), kind: ToastKind::Info, until: dur.map(|d| Instant::now() + d) });
    }

    // ------------------------------------------------------------ document edits

    /// Record an undo step before changing the document.
    pub fn checkpoint(&mut self) {
        self.undo.push(self.doc.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn changed(&mut self) {
        self.dirty_model = true;
        self.scene_dirty = true;
    }

    pub fn undo(&mut self) {
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return crate::asm_ui::undo(self);
        }
        if let Some(s) = &mut self.sketch {
            if let Some(d) = s.undo.pop() {
                s.redo.push(self.doc.clone());
                self.doc = d;
                self.after_sketch_edit();
                return;
            }
        }
        if let Some(d) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.doc, d));
            self.picks.clear();
            self.dialog = None;
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return crate::asm_ui::redo(self);
        }
        if let Some(s) = &mut self.sketch {
            if let Some(d) = s.redo.pop() {
                s.undo.push(self.doc.clone());
                self.doc = d;
                self.after_sketch_edit();
                return;
            }
        }
        if let Some(d) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.doc, d));
            self.picks.clear();
            self.dialog = None;
            self.changed();
        }
    }

    pub fn can_undo(&self) -> bool {
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return !self.asm.undo.is_empty();
        }
        !self.undo.is_empty() || self.sketch.as_ref().map(|s| !s.undo.is_empty()).unwrap_or(false)
    }

    pub fn can_redo(&self) -> bool {
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return !self.asm.redo.is_empty();
        }
        !self.redo.is_empty() || self.sketch.as_ref().map(|s| !s.redo.is_empty()).unwrap_or(false)
    }

    pub fn kernel_ready(&self) -> bool {
        matches!(self.kernel.state, KernelState::Ready | KernelState::Busy)
    }

    /// Start a feature from the toolbar.
    pub fn start_feature(&mut self, kind: &str) {
        if self.sketch.is_some() {
            return;
        }
        if self.dialog.is_some() {
            self.dialog_ok();
        }
        if kind == "sketch" {
            self.start_sketch();
            return;
        }
        let Some(mut op) = Op::new_default(kind) else { return };
        crate::dialog::seed_from_picks(&mut op, &self.picks, &self.doc);
        let before = self.doc.clone();
        self.checkpoint();
        let name = self.doc.next_name(kind);
        let id = self.doc.insert(name, op);
        self.picks.clear();
        self.dialog = Some(Dialog { feature: id, is_new: true, before, active_field: 0 });
        self.changed();
    }

    pub fn edit_feature(&mut self, id: FeatId) {
        if self.sketch.is_some() {
            return;
        }
        if self.dialog.is_some() {
            self.dialog_ok();
        }
        let Some(f) = self.doc.feature(id) else { return };
        if matches!(f.op, Op::Sketch { .. }) {
            self.edit_sketch(id);
            return;
        }
        let before = self.doc.clone();
        self.checkpoint();
        self.picks.clear();
        self.dialog = Some(Dialog { feature: id, is_new: false, before, active_field: 0 });
        self.scene_dirty = true;
    }

    pub fn dialog_ok(&mut self) {
        if let Some(d) = self.dialog.take() {
            if let Some(e) = self.errors.get(&d.feature) {
                self.flash(format!("{}: {e}", self.doc.feature(d.feature).map(|f| f.name.clone()).unwrap_or_default()), ToastKind::Error);
            }
        }
        self.picks.clear();
        self.scene_dirty = true;
    }

    pub fn dialog_cancel(&mut self) {
        if let Some(d) = self.dialog.take() {
            self.doc = d.before;
            self.undo.pop();
            self.changed();
        }
        self.picks.clear();
    }

    pub fn delete_feature(&mut self, id: FeatId) {
        if let Some(i) = self.doc.index_of(id) {
            self.checkpoint();
            self.doc.features.remove(i);
            if let Some(rb) = self.doc.rollback {
                if i < rb {
                    self.doc.rollback = Some(rb - 1);
                }
            }
            self.changed();
        }
    }

    pub fn new_document(&mut self) {
        let units = self.doc.units;
        self.load_workspace(crate::workspace::Workspace::default());
        self.doc.units = units;
        self.path = None;
    }

    /// Put the live Part Studio back into the workspace.
    pub fn sync_ws(&mut self) {
        if let Some(s) = self.ws.studios.get_mut(self.cur_studio) {
            s.doc = self.doc.clone();
        }
        self.ws.active = self.tab;
    }

    pub fn load_workspace(&mut self, ws: crate::workspace::Workspace) {
        self.ws = ws;
        if self.ws.studios.is_empty() {
            self.ws.studios.push(crate::workspace::Studio { name: "Part Studio 1".into(), doc: Document::default() });
        }
        self.cur_studio = 0;
        self.doc = self.ws.studios[0].doc.clone();
        self.result = RebuildResult::default();
        self.studio_results.clear();
        self.studio_undo.clear();
        self.undo.clear();
        self.redo.clear();
        self.picks.clear();
        self.dialog = None;
        self.sketch = None;
        self.asm = Default::default();
        self.dwg = Default::default();
        self.first_fit = true;
        self.tab = crate::workspace::Tab::Studio(0);
        self.changed();
        let active = self.ws.active;
        self.switch_tab(active);
    }

    /// Make `t` the visible tab; the Part Studio whose tab is shown becomes the live `doc`.
    pub fn switch_tab(&mut self, t: crate::workspace::Tab) {
        use crate::workspace::Tab;
        if self.sketch.is_some() {
            crate::sketcher::finish(self);
        }
        if self.dialog.is_some() {
            self.dialog_ok();
        }
        if let Tab::Studio(j) = t {
            if j != self.cur_studio && j < self.ws.studios.len() {
                let cur = self.cur_studio;
                self.ws.studios[cur].doc = self.doc.clone();
                self.studio_results.insert(cur, std::mem::take(&mut self.result));
                self.studio_undo.insert(cur, (std::mem::take(&mut self.undo), std::mem::take(&mut self.redo)));
                self.doc = self.ws.studios[j].doc.clone();
                self.result = self.studio_results.remove(&j).unwrap_or_default();
                let (u, r) = self.studio_undo.remove(&j).unwrap_or_default();
                self.undo = u;
                self.redo = r;
                self.cur_studio = j;
                self.errors = self.result.features.iter().filter_map(|f| f.error.clone().map(|e| (f.id, e))).collect();
                self.dirty_model = true;
            }
        }
        if matches!(t, Tab::Assembly(_) | Tab::Drawing(_)) {
            // every Part Studio needs geometry for the assembly/drawing
            for i in 0..self.ws.studios.len() {
                if i != self.cur_studio && !self.studio_results.contains_key(&i) {
                    let d = self.ws.studios[i].doc.clone();
                    self.kernel.rebuild(i, &d);
                }
            }
            self.asm.scene_dirty = true;
            self.dwg.dirty = true;
        }
        self.picks.clear();
        self.hover = None;
        self.tab = t;
        self.scene_dirty = true;
    }

    /// Meshes of a Part Studio's parts.
    pub fn studio_bodies(&self, studio: usize) -> &[crate::kernel::BodyMesh] {
        if studio == self.cur_studio { &self.result.bodies } else { self.studio_results.get(&studio).map(|r| r.bodies.as_slice()).unwrap_or(&[]) }
    }

    pub fn studio_doc(&self, studio: usize) -> Option<&Document> {
        if studio == self.cur_studio { Some(&self.doc) } else { self.ws.studios.get(studio).map(|s| &s.doc) }
    }

    // ------------------------------------------------------------ files

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Part", &["json"]).pick_file() {
            self.open_path(p);
        }
    }

    pub fn open_path(&mut self, p: std::path::PathBuf) {
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| crate::workspace::Workspace::from_json(&s)) {
            Ok(ws) => {
                self.load_workspace(ws);
                self.path = Some(p);
            }
            Err(e) => self.flash(format!("Could not open {}: {e}", p.display()), ToastKind::Error),
        }
    }

    pub fn save(&mut self, ask: bool) {
        let path = if ask || self.path.is_none() { rfd::FileDialog::new().add_filter("Part", &["json"]).set_file_name("part.json").save_file() } else { self.path.clone() };
        if let Some(p) = path {
            self.sync_ws();
            match std::fs::write(&p, self.ws.to_json()) {
                Ok(()) => {
                    self.flash_info(format!("Saved {}", p.display()), Some(Duration::from_secs(2)));
                    self.path = Some(p);
                }
                Err(e) => self.flash(format!("Could not save: {e}"), ToastKind::Error),
            }
        }
    }

    pub fn import_step(&mut self) {
        let Some(p) = rfd::FileDialog::new().add_filter("STEP / IGES", &["step", "stp", "STEP", "STP", "igs", "iges", "IGS", "IGES"]).pick_file() else { return };
        match std::fs::read_to_string(&p) {
            Ok(step) => {
                self.checkpoint();
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Import".into());
                self.doc.insert(name.clone(), Op::Import { name, step });
                self.first_fit = true;
                self.changed();
            }
            Err(e) => self.flash(format!("Could not read {}: {e}", p.display()), ToastKind::Error),
        }
    }

    pub fn export_obj(&mut self) {
        let Some(p) = rfd::FileDialog::new().add_filter("OBJ", &["obj"]).set_file_name("part.obj").save_file() else { return };
        let mut out = String::from("# cad-app\n");
        let mut base = 1usize;
        for b in &self.result.bodies {
            out.push_str(&format!("o {}\n", self.part_name(b.id).replace(' ', "_")));
            for v in b.positions.chunks(3) {
                out.push_str(&format!("v {} {} {}\n", v[0], v[1], v[2]));
            }
            for n in b.normals.chunks(3) {
                out.push_str(&format!("vn {} {} {}\n", n[0], n[1], n[2]));
            }
            for t in b.indices.chunks(3) {
                let (a, bb, c) = (t[0] as usize + base, t[1] as usize + base, t[2] as usize + base);
                out.push_str(&format!("f {a}//{a} {bb}//{bb} {c}//{c}\n"));
            }
            base += b.positions.len() / 3;
        }
        match std::fs::write(&p, out) {
            Ok(()) => self.flash_info(format!("Exported {}", p.display()), Some(Duration::from_secs(3))),
            Err(e) => self.flash(format!("Could not export: {e}"), ToastKind::Error),
        }
    }

    pub fn export(&mut self, format: &str) {
        let ext = match format {
            "step" => "step",
            "iges" => "igs",
            _ => "stl",
        };
        let Some(p) = rfd::FileDialog::new().add_filter(ext.to_uppercase(), &[ext]).set_file_name(format!("part.{ext}")).save_file() else { return };
        self.kernel.export(&self.doc, format, p.to_string_lossy().to_string());
        self.flash_info(format!("Exporting {}…", p.display()), None);
    }

    pub fn screenshot(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().add_filter("PNG", &["png"]).set_file_name("part.png").save_file() {
            self.screenshot_pending = Some(p);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_pending.clone() else { return };
        let img = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = img {
            self.screenshot_pending = None;
            let ppp = ctx.pixels_per_point();
            let r = self.viewport_rect;
            let region = img.region(&r, Some(ppp));
            let (w, h) = (region.size[0] as u32, region.size[1] as u32);
            let bytes: Vec<u8> = region.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::save_buffer(&path, &bytes, w, h, image::ColorType::Rgba8) {
                Ok(()) => self.flash_info(format!("Saved {}", path.display()), Some(Duration::from_secs(2))),
                Err(e) => self.flash(format!("Could not save screenshot: {e}"), ToastKind::Error),
            }
        }
    }

    // ------------------------------------------------------------ kernel

    fn pump_kernel(&mut self) {
        let was_loading = self.kernel.state == KernelState::Loading && self.kernel.crashes == 0;
        for o in self.kernel.poll() {
            match o {
                Outcome::Rebuilt(tag, r) if tag != self.cur_studio => {
                    self.studio_results.insert(tag, r);
                    self.asm.scene_dirty = true;
                    self.dwg.dirty = true;
                }
                Outcome::Rebuilt(_, r) => {
                    self.asm.scene_dirty = true;
                    self.dwg.dirty = true;
                    self.errors = r.features.iter().filter_map(|f| f.error.clone().map(|e| (f.id, e))).collect();
                    // keep sketch frames in the document in sync (not an undoable edit)
                    for f in &mut self.doc.features {
                        if let (Op::Sketch { frame, .. }, Some(fr)) = (&mut f.op, r.frames.get(&f.id)) {
                            *frame = *fr;
                        }
                    }
                    self.result = r;
                    self.scene_dirty = true;
                    if self.first_fit && !self.result.bodies.is_empty() {
                        self.first_fit = false;
                        self.fit_view();
                    }
                }
                Outcome::Projected(tag, key, views) => crate::drawing_ui::projected(self, tag, key, views),
                Outcome::Exported(err, path) => match err {
                    None => self.flash_info(format!("Exported {path}"), Some(Duration::from_secs(3))),
                    Some(e) => self.flash(format!("Export failed: {e}"), ToastKind::Error),
                },
            }
        }
        if was_loading && self.kernel_ready() {
            if self.toast.as_ref().map(|t| t.text.starts_with("Loading")).unwrap_or(false) {
                self.toast = None;
            }
        }
        if self.dirty_model && self.kernel.state != KernelState::Error {
            self.dirty_model = false;
            let cur = self.cur_studio;
            self.kernel.rebuild(cur, &self.doc);
        }
    }

    pub fn fit_view(&mut self) {
        let (lo, hi) = self.model_bounds();
        self.camera.fit(lo.as_vec3(), hi.as_vec3());
        self.scene_dirty = true;
    }

    pub fn model_bounds(&self) -> (glam::DVec3, glam::DVec3) {
        let mut lo = glam::DVec3::splat(f64::INFINITY);
        let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
        for b in &self.result.bodies {
            lo = lo.min(b.bbox[0].into());
            hi = hi.max(b.bbox[1].into());
        }
        for f in &self.doc.features {
            if let Op::Sketch { sketch, frame, .. } = &f.op {
                if let Some((a, b)) = sketch.bounds() {
                    for p in [a, b, glam::DVec2::new(a.x, b.y), glam::DVec2::new(b.x, a.y)] {
                        let w = frame.to_world(p);
                        lo = lo.min(w);
                        hi = hi.max(w);
                    }
                }
            }
        }
        if !lo.x.is_finite() {
            return (glam::DVec3::ZERO, glam::DVec3::splat(60.0));
        }
        (lo.min(glam::DVec3::ZERO), hi.max(glam::DVec3::ZERO))
    }

    // ------------------------------------------------------------ keyboard

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (ctrl, shift) = key_mods(ctx);
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if ctrl && pressed(Key::Z) {
            if shift {
                self.redo()
            } else {
                self.undo()
            }
            return;
        }
        if ctrl && pressed(Key::Y) {
            self.redo();
            return;
        }
        if ctrl && pressed(Key::S) {
            self.save(shift);
            return;
        }
        if ctrl && pressed(Key::O) {
            self.open_dialog();
            return;
        }
        if ctrl {
            return;
        }
        match self.tab {
            crate::workspace::Tab::Assembly(_) => return crate::asm_ui::shortcuts(self, ctx),
            crate::workspace::Tab::Drawing(_) => return crate::drawing_ui::shortcuts(self, ctx),
            _ => {}
        }
        if self.sketch.is_some() {
            crate::sketcher::shortcuts(self, ctx);
            return;
        }
        if pressed(Key::Escape) {
            if self.dialog.is_some() {
                self.dialog_cancel();
            } else if self.measure.is_some() {
                self.measure = None;
            } else {
                self.picks.clear();
            }
            self.scene_dirty = true;
        }
        if pressed(Key::Enter) && self.dialog.is_some() {
            self.dialog_ok();
        }
        if shift && self.kernel_ready() {
            for (k, kind) in [(Key::S, "sketch"), (Key::E, "extrude"), (Key::R, "revolve"), (Key::F, "fillet"), (Key::H, "hole")] {
                if pressed(k) {
                    self.start_feature(kind);
                }
            }
            return;
        }
        for (k, v) in [(Key::Num0, StdView::Iso), (Key::Num1, StdView::Front), (Key::Num2, StdView::Top), (Key::Num3, StdView::Right)] {
            if pressed(k) {
                self.camera.set_view(v);
                self.scene_dirty = true;
            }
        }
        if pressed(Key::F) {
            self.fit_view();
        }
        if pressed(Key::M) {
            self.toggle_measure();
        }
        if pressed(Key::Delete) || pressed(Key::Backspace) {
            // delete picked features from the tree? only bodies via feature
            if self.dialog.is_none() {
                let picked_feats: Vec<FeatId> = self.picks.iter().filter_map(|r| if let Ref::FeaturePlane { feature } = r { Some(*feature) } else { None }).collect();
                for f in picked_feats {
                    self.delete_feature(f);
                }
                self.picks.clear();
            }
        }
    }

    pub fn toggle_measure(&mut self) {
        if self.measure.is_some() {
            self.measure = None;
        } else {
            self.measure = Some(Measure { picks: self.picks.clone(), kept: vec![] });
            self.picks.clear();
        }
        self.scene_dirty = true;
    }

    // ------------------------------------------------------------ toolbar

    pub fn tool_button(&self, ui: &mut Ui, icon: &str, label: &str, tip: &str, primary: bool, active: bool, enabled: bool) -> egui::Response {
        let show_label = primary || self.phone;
        let fg = if active { theme::active_text() } else { theme::text() };
        let mut b = if show_label {
            egui::Button::image_and_text(icons::image(icon, 18.0).tint(fg), RichText::new(label).size(12.5).color(fg))
        } else {
            egui::Button::image(icons::image(icon, 18.0).tint(fg))
        };
        b = b.min_size(egui::vec2(26.0, 26.0));
        if active {
            b = b.fill(theme::active_fill()).stroke(Stroke::new(1.0, theme::active_stroke()));
        }
        let r = ui.add_enabled(enabled, b);
        r.on_hover_text(tip)
    }

    pub fn group(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
        ui.add_space(3.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 26.0), Sense::hover());
        ui.painter().vline(rect.center().x, rect.y_range().shrink(2.0), Stroke::new(1.0, theme::border()));
        ui.add_space(3.0);
        ui.spacing_mut().item_spacing.x = 1.0;
        add(ui);
        ui.spacing_mut().item_spacing.x = 1.0;
    }

    /// Top strip: logo, document menu and name, theme and phone toggles.
    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.add(icons::image("logo", 20.0).tint(theme::accent()));
            ui.label(RichText::new("cad-app").size(16.0).color(theme::muted()));
            ui.menu_image_button(icons::image("menu", 18.0).tint(theme::text()), |ui| self.document_menu(ui)).response.on_hover_text("Document menu");
            let name = self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled document".into());
            ui.label(RichText::new(name).size(15.0).strong().color(theme::text()));
            ui.label(RichText::new("Main").size(12.0).color(theme::muted()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (icon, tip) = if self.dark { ("sun", "Switch to light mode") } else { ("moon", "Switch to dark mode") };
                if ui.add(egui::Button::image(icons::image(icon, 17.0).tint(theme::text()))).on_hover_text(tip).clicked() {
                    self.set_dark(ui.ctx(), !self.dark);
                }
                let mode = if self.phone { ("desktop", "Switch to desktop mode") } else { ("phone", "Switch to phone mode") };
                if ui.add(egui::Button::image(icons::image(mode.0, 17.0).tint(theme::text()))).on_hover_text(mode.1).clicked() {
                    self.phone = !self.phone;
                    self.sidebar_open = !self.phone;
                }
                let (txt, col) = match self.kernel.state {
                    KernelState::Loading => ("Kernel loading", theme::muted()),
                    KernelState::Ready => ("Kernel ready", theme::ok_green()),
                    KernelState::Busy => ("Rebuilding", theme::accent()),
                    KernelState::Error => ("Kernel error", theme::error()),
                };
                ui.label(RichText::new(txt).size(12.0).color(theme::muted()));
                let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, col);
            });
        });
    }

    pub fn set_dark(&mut self, ctx: &egui::Context, dark: bool) {
        self.dark = dark;
        theme::set_dark(ctx, dark);
        save_dark_pref(dark);
        self.scene_dirty = true;
        self.asm.scene_dirty = true;
    }

    fn document_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(220.0);
        let mut action = None;
        for (a, icon, l) in [("new", "file", "New part"), ("open", "file", "Open .json…"), ("json", "download", "Save .json (Ctrl+S)"), ("import", "import", "Import STEP / IGES…")]
        {
            if ui.add(egui::Button::image_and_text(icons::image(icon, 15.0).tint(theme::text()), l)).clicked() {
                action = Some(a);
                ui.close();
            }
        }
        ui.menu_image_text_button(icons::image("download", 15.0).tint(theme::text()), "Export", |ui| {
            for (a, l) in [("step", "STEP"), ("iges", "IGES"), ("stl", "STL"), ("obj", "OBJ"), ("png", "Screenshot PNG")] {
                if ui.button(l).clicked() {
                    action = Some(a);
                    ui.close();
                }
            }
        });
        ui.menu_image_text_button(icons::image("view", 15.0).tint(theme::text()), "View", |ui| self.view_items(ui));
        ui.separator();
        let (icon, l) = if self.dark { ("sun", "Light mode") } else { ("moon", "Dark mode") };
        if ui.add(egui::Button::image_and_text(icons::image(icon, 15.0).tint(theme::text()), l)).clicked() {
            self.set_dark(ui.ctx(), !self.dark);
            ui.close();
        }
        match action {
            Some("new") => self.new_document(),
            Some("open") => self.open_dialog(),
            Some("json") => self.save(false),
            Some("import") => self.import_step(),
            Some(f @ ("step" | "stl" | "iges")) => self.export(f),
            Some("obj") => self.export_obj(),
            Some("png") => self.screenshot(ui.ctx()),
            _ => {}
        }
    }

    fn view_items(&mut self, ui: &mut Ui) {
        for (v, l) in [
            (StdView::Iso, "Isometric (0)"),
            (StdView::Front, "Front (1)"),
            (StdView::Top, "Top (2)"),
            (StdView::Right, "Right (3)"),
            (StdView::Back, "Back"),
            (StdView::Bottom, "Bottom"),
            (StdView::Left, "Left"),
        ] {
            if ui.button(l).clicked() {
                self.camera.set_view(v);
                self.scene_dirty = true;
                self.asm.scene_dirty = true;
                ui.close();
            }
        }
        if ui.button("Zoom to fit (F)").clicked() {
            self.fit_view();
            ui.close();
        }
    }

    /// The icon toolbar under the header.
    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(1.0, 4.0);
            if ui
                .add(egui::Button::image(icons::image("features", 18.0).tint(theme::text())).min_size(egui::vec2(26.0, 26.0)))
                .on_hover_text("Show or hide the feature list")
                .clicked()
            {
                self.sidebar_open = !self.sidebar_open;
            }
            ui.add_space(4.0);
            if ui
                .add_enabled(self.can_undo(), egui::Button::image(icons::image("undo", 18.0).tint(theme::text())).min_size(egui::vec2(26.0, 26.0)))
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.can_redo(), egui::Button::image(icons::image("redo", 18.0).tint(theme::text())).min_size(egui::vec2(26.0, 26.0)))
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                self.redo();
            }
            if !self.phone {
                match self.tab {
                    crate::workspace::Tab::Assembly(_) => crate::asm_ui::toolbar(self, ui),
                    crate::workspace::Tab::Drawing(_) => crate::drawing_ui::toolbar(self, ui),
                    _ if self.sketch.is_some() => crate::sketcher::toolbar(self, ui),
                    _ => self.model_tools(ui),
                }
            }
            if self.sketch.is_some() {
                Self::group(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let label = if self.phone { "Finish" } else { "Finish sketch" };
                    let b = egui::Button::image_and_text(icons::image("check", 18.0).tint(Color32::WHITE), RichText::new(label).color(Color32::WHITE).size(12.5))
                        .fill(theme::finish())
                        .min_size(egui::vec2(26.0, 26.0));
                    if ui.add(b).on_hover_text("Finish sketch (Enter)").clicked() {
                        crate::sketcher::finish(self);
                    }
                    if ui
                        .add(egui::Button::image(icons::image("close", 18.0).tint(theme::text())).min_size(egui::vec2(26.0, 26.0)))
                        .on_hover_text("Discard this sketch session's changes")
                        .clicked()
                    {
                        crate::sketcher::cancel(self);
                    }
                });
            } else if matches!(self.tab, crate::workspace::Tab::Studio(_)) && !self.phone {
                if ui.available_width() < 200.0 {
                    ui.end_row();
                }
                ui.add_space(8.0);
                self.tool_search(ui);
            }
        });
    }

    /// "Search tools…" box: type part of a feature name and press Enter (or pick
    /// from the list) to start it.
    fn tool_search(&mut self, ui: &mut Ui) {
        let q = self.tool_search.trim().to_lowercase();
        let matches: Vec<&str> =
            if q.is_empty() { vec![] } else { FEATURE_GROUPS.iter().flat_map(|g| g.iter().copied()).filter(|k| doc::kind_label(k).to_lowercase().contains(&q)).collect() };
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.tool_search).hint_text(RichText::new("Search tools…").color(theme::muted())).desired_width(170.0).margin(egui::vec2(6.0, 4.0)),
        );
        let mut start = None;
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            start = matches.first().copied();
        }
        let mut list_hovered = false;
        if !matches.is_empty() {
            let area =
                egui::Area::new(egui::Id::new("tool-search-list")).order(egui::Order::Foreground).fixed_pos(resp.rect.left_bottom() + egui::vec2(0.0, 2.0)).show(ui.ctx(), |ui| {
                    Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(resp.rect.width());
                        for k in &matches {
                            if ui.add(egui::Button::image_and_text(icons::image(k, 16.0).tint(theme::text()), doc::kind_label(k)).frame(false)).clicked() {
                                start = Some(*k);
                            }
                        }
                    });
                });
            list_hovered = area.response.contains_pointer();
        }
        // clicking anywhere else closes the list
        if start.is_none() && !resp.has_focus() && !list_hovered && ui.input(|i| i.pointer.any_click()) {
            self.tool_search.clear();
        }
        if let Some(k) = start {
            self.tool_search.clear();
            if self.kernel_ready() {
                self.start_feature(k);
            }
        }
    }

    pub fn model_tools(&mut self, ui: &mut Ui) {
        let ready = self.kernel_ready();
        for group in FEATURE_GROUPS {
            Self::group(ui, |ui| {
                for kind in *group {
                    let label = doc::kind_label(kind);
                    let tip = match shortcut(kind) {
                        Some(s) => format!("{label} ({s})"),
                        None => label.to_string(),
                    };
                    if self.tool_button(ui, kind, label, &tip, PRIMARY.contains(kind), false, ready).clicked() {
                        self.start_feature(kind);
                    }
                }
            });
        }
    }

    /// View toggles for the phone dock (the desktop has them beside the viewport).
    fn view_toggles(&mut self, ui: &mut Ui) {
        Self::group(ui, |ui| {
            if self.tool_button(ui, "dimension", "Dims", "Show dimensions on the model", false, self.show_dims, true).clicked() {
                self.show_dims = !self.show_dims;
                self.scene_dirty = true;
            }
            if self.tool_button(ui, "xray", "X-ray", "See-through bodies (X-ray)", false, self.xray, true).clicked() {
                self.xray = !self.xray;
                self.scene_dirty = true;
            }
            if self.tool_button(ui, "measure", "Measure", "Measure (M)", false, self.measure.is_some(), true).clicked() {
                self.toggle_measure();
            }
            self.section_ui(ui);
        });
    }

    pub fn section_ui(&mut self, ui: &mut Ui) {
        let label = |s: &Option<Section>| match s {
            None => "No section".to_string(),
            Some(s) => format!("Section: {}{}", ["Top", "Front", "Right"][s.plane as usize], if s.flip { " (flipped)" } else { "" }),
        };
        let mut sec = self.section;
        egui::ComboBox::from_id_salt("section")
            .selected_text(label(&sec))
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sec, None, "No section");
                for p in 0..3u8 {
                    for flip in [false, true] {
                        let off = self.section.map(|s| s.offset).unwrap_or(0.0);
                        let v = Some(Section { plane: p, flip, offset: off });
                        let l = label(&v);
                        if ui.selectable_label(sec.map(|s| s.plane == p && s.flip == flip).unwrap_or(false), l).clicked() {
                            sec = v;
                        }
                    }
                }
            })
            .response
            .on_hover_text("Section view");
        if sec != self.section {
            self.section = sec;
            self.scene_dirty = true;
        }
        if let Some(s) = &mut self.section {
            let mut v = s.offset;
            if ui.add(egui::DragValue::new(&mut v).speed(0.5)).on_hover_text("Section offset (mm)").changed() {
                s.offset = v;
                self.scene_dirty = true;
            }
        }
    }

    // ------------------------------------------------------------ sidebar

    /// Far-left icon rail choosing what the left panel shows.
    fn rail(&mut self, ui: &mut Ui) {
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (pane, icon, tip) in
                [(LeftPane::Features, "features", "Feature list"), (LeftPane::Variables, "variable", "Variables"), (LeftPane::Configurations, "configurations", "Configurations")]
            {
                let on = self.sidebar_open && self.left_pane == pane;
                let fg = if on { theme::active_text() } else { theme::text() };
                let mut b = egui::Button::image(icons::image(icon, 19.0).tint(fg)).min_size(egui::vec2(30.0, 30.0));
                if on {
                    b = b.fill(theme::active_fill());
                }
                if ui.add(b).on_hover_text(tip).clicked() {
                    if on {
                        self.sidebar_open = false;
                    } else {
                        self.left_pane = pane;
                        self.sidebar_open = true;
                    }
                }
            }
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        match self.left_pane {
            LeftPane::Features => {
                // features on top, parts pinned below, like Onshape
                let total = ui.available_height();
                let parts_h = if self.parts_open { (24.0 + 23.0 * self.result.bodies.len().max(1) as f32 + 12.0).min(total * 0.4) } else { 30.0 };
                let props = self.picks.iter().any(|r| matches!(r, Ref::Body { .. }));
                let props_h = if props { 130.0 } else { 0.0 };
                let top_h = (total - parts_h - props_h).max(80.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), top_h), |ui| {
                    egui::ScrollArea::vertical().id_salt("feature-scroll").auto_shrink([false, false]).show(ui, |ui| {
                        Frame::new().inner_margin(Margin { left: 6, right: 6, top: 6, bottom: 6 }).show(ui, |ui| self.feature_tree(ui));
                    });
                });
                let y = ui.cursor().top();
                ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, theme::border()));
                Frame::new().inner_margin(Margin { left: 6, right: 6, top: 4, bottom: 2 }).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("parts-scroll").max_height(parts_h - 6.0).auto_shrink([false, true]).show(ui, |ui| self.parts_list(ui));
                });
                if props {
                    section(ui, "Mass properties", |ui| self.properties(ui));
                }
            }
            LeftPane::Variables => {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| section(ui, "Variables", |ui| self.variables(ui)));
            }
            LeftPane::Configurations => {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| section(ui, "Configurations", |ui| self.configurations(ui)));
            }
        }
    }

    fn feature_tree(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 1.0;
        // filter box
        ui.horizontal(|ui| {
            ui.add(icons::image("filter", 16.0).tint(theme::muted()));
            ui.add(
                egui::TextEdit::singleline(&mut self.tree_filter)
                    .hint_text(RichText::new("Filter by name or type").italics().color(theme::muted()))
                    .desired_width(ui.available_width()),
            );
        });
        ui.add_space(6.0);
        // header
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Features ({})", self.doc.features.len())).strong().size(13.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let rolled = self.doc.rollback.is_some();
                if ui
                    .add(egui::Button::image(icons::image("rollback", 15.0).tint(if rolled { theme::accent() } else { theme::text() })))
                    .on_hover_text(if rolled { "Roll to end" } else { "Roll back one feature" })
                    .clicked()
                {
                    self.checkpoint();
                    let n = self.doc.features.len();
                    self.doc.rollback = if rolled {
                        None
                    } else if n > 0 {
                        Some(n - 1)
                    } else {
                        None
                    };
                    self.changed();
                }
            });
        });
        ui.add_space(2.0);
        let filter = self.tree_filter.trim().to_lowercase();
        let shown = |name: &str, kind: &str| filter.is_empty() || name.to_lowercase().contains(&filter) || kind.to_lowercase().contains(&filter);

        // default geometry
        if filter.is_empty() || "default geometry origin top front right plane".contains(&filter) {
            if disclosure_row(ui, "Default geometry", self.geom_open).clicked() {
                self.geom_open = !self.geom_open;
            }
            if self.geom_open {
                tree_row(ui, 1, "origin", "Origin", false, false, false, false, None).on_hover_text("Origin (0, 0, 0)");
                for p in OriginPlane::ALL {
                    let r = Ref::OriginPlane { plane: p };
                    let picked = self.picks.contains(&r);
                    let resp = tree_row(ui, 1, "planeSheet", p.name().trim_end_matches(" plane"), picked, false, false, false, None);
                    if resp.clicked() {
                        self.pick_from_tree(r.clone(), ui.input(|i| i.modifiers.shift || i.modifiers.command));
                    }
                    if resp.hovered() && self.hover.as_ref() != Some(&r) {
                        self.hover = Some(r);
                        self.scene_dirty = true;
                    }
                }
            }
        }

        let rb = self.doc.rollback_index();
        let mut action: Option<(&'static str, FeatId)> = None;
        let mut drop_rollback: Option<usize> = None;
        let n = self.doc.features.len();
        for i in 0..=n {
            if i == rb {
                let resp = rollbar(ui, self.doc.rollback.is_some());
                if resp.clicked() {
                    drop_rollback = Some(usize::MAX);
                }
            }
            if i == n {
                break;
            }
            let f = &self.doc.features[i];
            let id = f.id;
            let editing = self.dialog.as_ref().map(|d| d.feature == id).unwrap_or(false) || self.sketch.as_ref().map(|s| s.feature == id).unwrap_or(false);
            if let Some((rid, text)) = &mut self.renaming {
                if *rid == id {
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width()));
                    r.request_focus();
                    if r.lost_focus() {
                        let t = text.trim().to_string();
                        let rid = *rid;
                        self.renaming = None;
                        if !t.is_empty() {
                            self.checkpoint();
                            if let Some(f) = self.doc.feature_mut(rid) {
                                f.name = t;
                            }
                        }
                    }
                    continue;
                }
            }
            if !shown(&f.name, doc::kind_label(f.op.kind())) {
                continue;
            }
            let picked = self.picks.iter().any(|r| matches!(r, Ref::FeaturePlane { feature } if *feature == id));
            let err = self.errors.get(&id).cloned();
            let resp = tree_row(ui, 0, f.op.kind(), &f.name, picked || editing, i >= rb, f.suppressed, f.hidden, err.as_deref());
            let resp = resp.on_hover_text(match &err {
                Some(e) => format!("{}: {e}", f.name),
                None => "Double-click to edit · right-click for options".into(),
            });
            if resp.double_clicked() {
                action = Some(("edit", id));
            } else if resp.clicked() {
                if self.dialog.is_some() {
                    if self.dialog.as_ref().map(|d| d.feature != id).unwrap_or(false) {
                        crate::dialog::add_pick(self, Ref::Feature { feature: id });
                    }
                } else if matches!(f.op, Op::Sketch { .. } | Op::Plane { .. }) {
                    self.pick_from_tree(Ref::FeaturePlane { feature: id }, ui.input(|i| i.modifiers.shift || i.modifiers.command));
                }
            }
            resp.context_menu(|ui| {
                ui.set_min_width(200.0);
                if ui.button("Edit").clicked() {
                    action = Some(("edit", id));
                    ui.close();
                }
                if ui.button("Rename").clicked() {
                    action = Some(("rename", id));
                    ui.close();
                }
                if ui.button(if self.doc.features[i].suppressed { "Unsuppress" } else { "Suppress" }).clicked() {
                    action = Some(("suppress", id));
                    ui.close();
                }
                if ui.button(if self.doc.features[i].hidden { "Show" } else { "Hide" }).clicked() {
                    action = Some(("hide", id));
                    ui.close();
                }
                if ui.button("Roll back to here").clicked() {
                    drop_rollback = Some(i + 1);
                    ui.close();
                }
                if i > 0 && ui.button("Move up").clicked() {
                    action = Some(("up", id));
                    ui.close();
                }
                if i + 1 < n && ui.button("Move down").clicked() {
                    action = Some(("down", id));
                    ui.close();
                }
                ui.separator();
                if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                    action = Some(("delete", id));
                    ui.close();
                }
            });
        }
        if let Some(r) = drop_rollback {
            self.checkpoint();
            self.doc.rollback = if r == usize::MAX || r >= n { None } else { Some(r) };
            self.changed();
        }
        if let Some((a, id)) = action {
            match a {
                "edit" => self.edit_feature(id),
                "rename" => self.renaming = self.doc.feature(id).map(|f| (id, f.name.clone())),
                "suppress" | "hide" => {
                    self.checkpoint();
                    if let Some(f) = self.doc.feature_mut(id) {
                        if a == "suppress" {
                            f.suppressed = !f.suppressed;
                        } else {
                            f.hidden = !f.hidden;
                        }
                    }
                    self.changed();
                }
                "up" | "down" => {
                    if let Some(i) = self.doc.index_of(id) {
                        let j = if a == "up" { i - 1 } else { i + 1 };
                        self.checkpoint();
                        self.doc.features.swap(i, j);
                        self.changed();
                    }
                }
                "delete" => self.delete_feature(id),
                _ => {}
            }
        }
    }

    pub fn part_props(&self, id: doc::BodyId) -> doc::PartProps {
        self.doc.parts.get(&doc::body_key(id)).cloned().unwrap_or_default()
    }

    pub fn part_name(&self, id: doc::BodyId) -> String {
        let p = self.part_props(id);
        p.name.unwrap_or_else(|| self.result.bodies.iter().find(|b| b.id == id).map(|b| b.name.clone()).unwrap_or_default())
    }

    pub fn part_color(&self, id: doc::BodyId) -> Color32 {
        self.part_props(id).color.map(|c| Color32::from_rgb(c[0], c[1], c[2])).unwrap_or(theme::vp::body())
    }

    fn set_part(&mut self, id: doc::BodyId, f: impl FnOnce(&mut doc::PartProps)) {
        self.checkpoint();
        let key = doc::body_key(id);
        let mut p = self.doc.parts.get(&key).cloned().unwrap_or_default();
        f(&mut p);
        if p == doc::PartProps::default() {
            self.doc.parts.remove(&key);
        } else {
            self.doc.parts.insert(key, p);
        }
        self.scene_dirty = true;
    }

    fn parts_list(&mut self, ui: &mut Ui) {
        if disclosure_row(ui, &format!("Parts ({})", self.result.bodies.len()), self.parts_open).clicked() {
            self.parts_open = !self.parts_open;
        }
        if !self.parts_open {
            return;
        }
        if self.result.bodies.is_empty() {
            ui.label(RichText::new("    No parts yet").color(theme::muted()).size(12.0));
            return;
        }
        ui.spacing_mut().item_spacing.y = 1.0;
        let ids: Vec<doc::BodyId> = self.result.bodies.iter().map(|b| b.id).collect();
        for id in ids {
            let r = Ref::Body { body: id };
            let picked = self.picks.contains(&r);
            let name = self.part_name(id);
            if let Some((key, text)) = &mut self.renaming_part {
                if *key == doc::body_key(id) {
                    let resp = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width()));
                    resp.request_focus();
                    if resp.lost_focus() {
                        let t = text.trim().to_string();
                        self.renaming_part = None;
                        self.set_part(id, |p| p.name = (!t.is_empty()).then_some(t));
                    }
                    continue;
                }
            }
            let resp = tree_row(ui, 1, "part", &name, picked, false, false, false, None);
            // colour swatch
            let sw = egui::Rect::from_center_size(egui::pos2(resp.rect.right() - 12.0, resp.rect.center().y), egui::vec2(10.0, 10.0));
            ui.painter().rect_filled(sw, 2.0, self.part_color(id));
            if resp.clicked() {
                self.pick_from_tree(r, ui.input(|i| i.modifiers.shift || i.modifiers.command));
            }
            if resp.double_clicked() {
                self.renaming_part = Some((doc::body_key(id), name.clone()));
            }
            resp.context_menu(|ui| {
                ui.set_min_width(200.0);
                if ui.button("Rename").clicked() {
                    self.renaming_part = Some((doc::body_key(id), name.clone()));
                    ui.close();
                }
                ui.menu_button("Appearance", |ui| {
                    let mut c = self.part_color(id);
                    if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
                        self.set_part(id, |p| p.color = Some([c.r(), c.g(), c.b()]));
                    }
                    if ui.button("Default").clicked() {
                        self.set_part(id, |p| p.color = None);
                    }
                });
                ui.menu_button("Material", |ui| {
                    let cur = self.part_props(id).material;
                    if ui.selectable_label(cur.is_none(), "None").clicked() {
                        self.set_part(id, |p| p.material = None);
                    }
                    for (m, _) in doc::MATERIALS {
                        if ui.selectable_label(cur.as_deref() == Some(m), *m).clicked() {
                            self.set_part(id, |p| p.material = Some(m.to_string()));
                        }
                    }
                });
            });
        }
    }

    fn properties(&mut self, ui: &mut Ui) {
        let units = self.doc.units;
        let ids: Vec<doc::BodyId> = self.picks.iter().filter_map(|r| if let Ref::Body { body } = r { Some(*body) } else { None }).collect();
        let bodies: Vec<&crate::kernel::BodyMesh> = self.result.bodies.iter().filter(|b| ids.contains(&b.id)).collect();
        if bodies.is_empty() {
            return;
        }
        let vol: f64 = bodies.iter().map(|b| b.volume).sum();
        let area: f64 = bodies.iter().map(|b| b.area).sum();
        let mut mass = 0.0;
        let mut all_mass = true;
        let mut c = glam::DVec3::ZERO;
        for b in &bodies {
            c += glam::DVec3::from(b.centroid) * b.volume;
            match self.part_props(b.id).material.and_then(|m| doc::MATERIALS.iter().find(|x| x.0 == m).map(|x| x.1)) {
                Some(d) => mass += b.volume / 1000.0 * d,
                None => all_mass = false,
            }
        }
        let c = if vol > 0.0 { c / vol } else { c };
        let mut lo = glam::DVec3::splat(f64::INFINITY);
        let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
        for b in &bodies {
            lo = lo.min(b.bbox[0].into());
            hi = hi.max(b.bbox[1].into());
        }
        let size = hi - lo;
        let row = |ui: &mut Ui, k: &str, v: String| {
            ui.label(RichText::new(k).color(theme::muted()).size(12.0));
            ui.label(RichText::new(v).size(12.0));
            ui.end_row();
        };
        egui::Grid::new("props").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
            row(ui, "Volume", fmt_volume(vol, units));
            row(ui, "Surface area", fmt_area(area, units));
            row(ui, "Mass", if all_mass { format!("{} g", crate::expr::trim(mass, 2)) } else { "set a material".into() });
            row(ui, "Centre of mass", format!("{}, {}, {}", units.fmt(c.x), units.fmt(c.y), units.fmt(c.z)));
            row(ui, "Size", format!("{} × {} × {} {}", units.fmt(size.x), units.fmt(size.y), units.fmt(size.z), units.label()));
        });
    }

    pub fn pick_from_tree(&mut self, r: Ref, add: bool) {
        if self.dialog.is_some() {
            crate::dialog::add_pick(self, r);
            return;
        }
        if !add {
            self.picks.clear();
        }
        if let Some(i) = self.picks.iter().position(|x| *x == r) {
            self.picks.remove(i);
        } else {
            self.picks.push(r);
        }
        self.scene_dirty = true;
    }

    fn configurations(&mut self, ui: &mut Ui) {
        let names: Vec<String> = self.doc.configs.iter().map(|c| c.name.clone()).collect();
        let mut active = self.doc.active_config;
        let label = active.and_then(|i| names.get(i)).cloned().unwrap_or_else(|| "Default".into());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("config").selected_text(label).width(130.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut active, None, "Default");
                for (i, n) in names.iter().enumerate() {
                    ui.selectable_value(&mut active, Some(i), n);
                }
            });
            if ui.button("New").on_hover_text("New configuration with the current values").clicked() {
                self.checkpoint();
                let n = self.doc.configs.len() + 1;
                self.doc.configs.push(crate::doc::Config { name: format!("Config {n}"), values: Default::default() });
                active = Some(self.doc.configs.len() - 1);
            }
            if let Some(i) = active {
                if ui.button("Delete").clicked() {
                    self.checkpoint();
                    self.doc.configs.remove(i);
                    active = None;
                }
            }
        });
        if let Some(i) = active {
            if let Some(c) = self.doc.configs.get_mut(i) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Name").color(theme::muted()).size(12.0));
                    ui.text_edit_singleline(&mut c.name);
                });
            }
            ui.label(RichText::new("Variable edits now apply to this configuration only.").color(theme::muted()).size(12.0));
        } else {
            ui.label(RichText::new("Configurations are named sets of variable values.").color(theme::muted()).size(12.0));
        }
        if active != self.doc.active_config {
            self.doc.active_config = active;
            self.changed();
        }
    }

    fn variables(&mut self, ui: &mut Ui) {
        let (_, errs) = self.doc.var_values();
        let mut remove = None;
        egui::Grid::new("vars").num_columns(3).spacing([6.0, 4.0]).show(ui, |ui| {
            let cfg = self.doc.active_config.and_then(|c| self.doc.configs.get(c)).cloned();
            for (i, v) in self.doc.variables.clone().iter().enumerate() {
                let v = &crate::doc::Variable { name: v.name.clone(), expr: cfg.as_ref().and_then(|c| c.values.get(&v.name)).cloned().unwrap_or_else(|| v.expr.clone()) };
                ui.label(RichText::new(&v.name).monospace());
                let editing = self.editing_var.as_ref().map(|(k, _)| *k == i).unwrap_or(false);
                if editing {
                    let (_, text) = self.editing_var.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(90.0));
                    r.request_focus();
                    if r.lost_focus() {
                        let t = text.clone();
                        self.editing_var = None;
                        self.checkpoint();
                        match self.doc.active_config {
                            // in a configuration, edits override the value for that configuration only
                            Some(c) if c < self.doc.configs.len() => {
                                let name = self.doc.variables[i].name.clone();
                                self.doc.configs[c].values.insert(name, t);
                            }
                            _ => self.doc.variables[i].expr = t,
                        }
                        self.changed();
                    }
                } else {
                    let mut txt = RichText::new(&v.expr).monospace();
                    if errs.get(i).map(|e| e.is_some()).unwrap_or(false) {
                        txt = txt.color(theme::error_soft());
                    }
                    let r = ui.add(egui::Label::new(txt).sense(Sense::click())).on_hover_text(format!("{}: double-click to change", v.name));
                    if r.double_clicked() {
                        self.editing_var = Some((i, v.expr.clone()));
                    }
                }
                if ui.small_button("×").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
            ui.add(egui::TextEdit::singleline(&mut self.var_name).hint_text("name").desired_width(90.0));
            ui.add(egui::TextEdit::singleline(&mut self.var_value).hint_text("value").desired_width(90.0));
            let valid = is_ident(&self.var_name) && !self.var_value.trim().is_empty() && !self.doc.variables.iter().any(|v| v.name == self.var_name);
            if ui.add_enabled(valid, egui::Button::new("Add")).clicked() {
                self.checkpoint();
                self.doc.variables.push(doc::Variable { name: self.var_name.trim().into(), expr: self.var_value.trim().into() });
                self.var_name.clear();
                self.var_value.clear();
                self.changed();
            }
            ui.end_row();
        });
        if let Some(i) = remove {
            self.checkpoint();
            self.doc.variables.remove(i);
            self.changed();
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new("Use variables in any dimension or feature value, e.g. ").color(theme::muted()).size(12.0));
            ui.label(RichText::new("width / 2").monospace().size(11.5));
            ui.label(RichText::new(".").color(theme::muted()).size(12.0));
        });
    }

    // ------------------------------------------------------------ status bar

    fn hint_text(&self) -> String {
        if let crate::workspace::Tab::Assembly(_) = self.tab {
            if self.asm.dialog.is_some() {
                "Click a face, edge or circular edge on each of the two parts · Enter to accept".to_string()
            } else {
                "Insert parts, then pick a mate type · drag parts to move them, Shift+drag rotates".to_string()
            }
        } else if let crate::workspace::Tab::Drawing(_) = self.tab {
            "Insert views, add dimensions and notes, then export".to_string()
        } else if let Some(s) = &self.sketch {
            crate::sketcher::status_text(self, s)
        } else if let Some(m) = &self.measure {
            crate::viewport::measure_text(self, m)
        } else if self.dialog.is_some() {
            "Pick geometry in the viewport for the highlighted field · Enter to accept, Esc to cancel".to_string()
        } else if !self.picks.is_empty() {
            format!("{} selected · start a feature, or Esc to clear", self.picks.len())
        } else {
            "Click faces, edges, planes or sketch regions to select them before starting a feature".to_string()
        }
    }

    /// The hint line, drawn over the bottom-left of the work area.
    fn hint_ui(&self, ctx: &egui::Context, area: egui::Rect) {
        let msg = self.hint_text();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("hint")));
        let font = egui::FontId::proportional(12.0);
        let g = painter.layout(msg, font, theme::muted(), area.width() - 260.0);
        let at = egui::pos2(area.left() + 12.0, area.bottom() - 10.0 - g.size().y);
        painter.rect_filled(egui::Rect::from_min_size(at, g.size()).expand2(egui::vec2(6.0, 3.0)), 3.0, theme::label_bg());
        painter.galley(at, g, theme::muted());
    }

    fn units_combo(&mut self, ui: &mut Ui) {
        let mut u = self.doc.units;
        egui::ComboBox::from_id_salt("units")
            .selected_text(u.label())
            .width(52.0)
            .show_ui(ui, |ui| {
                for x in Units::ALL {
                    ui.selectable_value(&mut u, x, x.label());
                }
            })
            .response
            .on_hover_text("Document units: plain numbers you type and all lengths shown use this unit");
        if u != self.doc.units {
            self.checkpoint();
            self.doc.units = u;
            self.changed();
        }
    }

    fn toast_ui(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.toast else { return };
        if t.until.map(|u| Instant::now() > u).unwrap_or(false) {
            self.toast = None;
            return;
        }
        if let Some(u) = t.until {
            ctx.request_repaint_after(u.saturating_duration_since(Instant::now()));
        }
        let r = self.viewport_rect;
        let (fill, stroke) = match t.kind {
            ToastKind::Info => (theme::toast(), theme::border()),
            ToastKind::Error => (theme::toast_err(), theme::toast_err_border()),
        };
        let text = t.text.clone();
        let bottom = if self.phone { 70.0 } else { 14.0 };
        egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(egui::pos2(r.center().x, r.bottom() - bottom))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke)).corner_radius(6).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                    ui.set_max_width(r.width() * 0.8);
                    ui.label(RichText::new(text).color(theme::text()));
                });
            });
    }

    fn tabs_ui(&mut self, ui: &mut Ui) {
        use crate::workspace::Tab;
        let mut tabs: Vec<(Tab, String, &str)> = vec![];
        for (i, s) in self.ws.studios.iter().enumerate() {
            tabs.push((Tab::Studio(i), s.name.clone(), "partStudio"));
        }
        for (i, a) in self.ws.assemblies.iter().enumerate() {
            tabs.push((Tab::Assembly(i), a.name.clone(), "assembly"));
        }
        for (i, d) in self.ws.drawings.iter().enumerate() {
            tabs.push((Tab::Drawing(i), d.name.clone(), "drawing"));
        }
        let mut switch = None;
        let mut action: Option<(&str, Tab)> = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.menu_image_button(icons::image("plus", 16.0).tint(theme::text()), |ui| {
                if ui.add(egui::Button::image_and_text(icons::image("partStudio", 15.0).tint(theme::text()), "Part Studio")).clicked() {
                    action = Some(("new-studio", Tab::Studio(0)));
                    ui.close();
                }
                if ui.add(egui::Button::image_and_text(icons::image("assembly", 15.0).tint(theme::text()), "Assembly")).clicked() {
                    action = Some(("new-assembly", Tab::Studio(0)));
                    ui.close();
                }
                if ui.add(egui::Button::image_and_text(icons::image("drawing", 15.0).tint(theme::text()), "Drawing")).clicked() {
                    action = Some(("new-drawing", Tab::Studio(0)));
                    ui.close();
                }
            })
            .response
            .on_hover_text("Add a Part Studio, Assembly or Drawing");
            ui.add_space(6.0);
            for (t, name, icon) in tabs {
                if let Some((rt, text)) = &mut self.renaming_tab {
                    if *rt == t {
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                        r.request_focus();
                        if r.lost_focus() {
                            action = Some(("rename-done", t));
                        }
                        continue;
                    }
                }
                let active = self.tab == t;
                let r = tab_button(ui, icon, &name, active);
                if r.clicked() {
                    switch = Some(t);
                }
                if r.double_clicked() {
                    action = Some(("rename", t));
                }
                r.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        action = Some(("rename", t));
                        ui.close();
                    }
                    if ui.button("Duplicate").clicked() {
                        action = Some(("duplicate", t));
                        ui.close();
                    }
                    if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                        action = Some(("delete", t));
                        ui.close();
                    }
                });
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_space(4.0);
                self.units_combo(ui);
                ui.menu_button(RichText::new(format!("Versions ({})", self.ws.versions.len())).size(12.0), |ui| {
                    ui.set_min_width(260.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.version_name).hint_text("Version name").desired_width(150.0));
                        if ui.button("Create").clicked() {
                            action = Some(("version", Tab::Studio(0)));
                            ui.close();
                        }
                    });
                    ui.separator();
                    if self.ws.versions.is_empty() {
                        ui.label(RichText::new("No versions yet").color(theme::muted()));
                    }
                    for (i, v) in self.ws.versions.iter().enumerate().rev() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&v.name).strong());
                            ui.label(RichText::new(&v.created).color(theme::muted()).size(11.0));
                            if ui.small_button("Restore").on_hover_text("Go back to this version (the current state stays in undo)").clicked() {
                                self.restore_version = Some(i);
                                ui.close();
                            }
                        });
                    }
                });
            });
        });
        if let Some(t) = switch {
            if t != self.tab {
                self.switch_tab(t);
            }
        }
        if let Some((a, t)) = action {
            self.tab_action(a, t);
        }
        if let Some(i) = self.restore_version.take() {
            self.sync_ws();
            let mut ws = self.ws.clone();
            match ws.restore(i) {
                Ok(()) => {
                    let path = self.path.clone();
                    self.load_workspace(ws);
                    self.path = path;
                    self.flash_info("Restored the version", Some(Duration::from_secs(2)));
                }
                Err(e) => self.flash(format!("Could not restore: {e}"), ToastKind::Error),
            }
        }
    }

    fn tab_action(&mut self, a: &str, t: crate::workspace::Tab) {
        use crate::workspace::Tab;
        let name_of = |ws: &crate::workspace::Workspace, t: Tab| match t {
            Tab::Studio(i) => ws.studios.get(i).map(|s| s.name.clone()),
            Tab::Assembly(i) => ws.assemblies.get(i).map(|s| s.name.clone()),
            Tab::Drawing(i) => ws.drawings.get(i).map(|s| s.name.clone()),
        };
        match a {
            "rename" => self.renaming_tab = name_of(&self.ws, t).map(|n| (t, n)),
            "rename-done" => {
                if let Some((_, text)) = self.renaming_tab.take() {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        match t {
                            Tab::Studio(i) => self.ws.studios[i].name = text,
                            Tab::Assembly(i) => self.ws.assemblies[i].name = text,
                            Tab::Drawing(i) => self.ws.drawings[i].name = text,
                        }
                    }
                }
            }
            "version" => {
                self.sync_ws();
                let name = if self.version_name.trim().is_empty() { format!("V{}", self.ws.versions.len() + 1) } else { self.version_name.trim().to_string() };
                self.ws.make_version(name.clone());
                self.version_name.clear();
                self.flash_info(format!("Created version {name}"), Some(Duration::from_secs(2)));
            }
            "new-studio" => {
                self.sync_ws();
                let n = self.ws.studios.len() + 1;
                self.ws.studios.push(crate::workspace::Studio { name: format!("Part Studio {n}"), doc: Document { units: self.doc.units, ..Default::default() } });
                self.switch_tab(Tab::Studio(n - 1));
            }
            "new-assembly" => {
                let n = self.ws.assemblies.len() + 1;
                self.ws.assemblies.push(crate::assembly::Assembly { name: format!("Assembly {n}"), ..Default::default() });
                self.switch_tab(Tab::Assembly(n - 1));
                self.asm.insert_open = true;
            }
            "new-drawing" => {
                let n = self.ws.drawings.len() + 1;
                self.ws.drawings.push(crate::drawing::Drawing { name: format!("Drawing {n}"), sheet: "A3".into(), ..Default::default() });
                self.switch_tab(Tab::Drawing(n - 1));
                crate::drawing_ui::add_standard_views(self);
            }
            "duplicate" => {
                self.sync_ws();
                match t {
                    Tab::Studio(i) => {
                        let mut s = self.ws.studios[i].clone();
                        s.name = format!("{} copy", s.name);
                        self.ws.studios.push(s);
                    }
                    Tab::Assembly(i) => {
                        let mut s = self.ws.assemblies[i].clone();
                        s.name = format!("{} copy", s.name);
                        self.ws.assemblies.push(s);
                    }
                    Tab::Drawing(i) => {
                        let mut s = self.ws.drawings[i].clone();
                        s.name = format!("{} copy", s.name);
                        self.ws.drawings.push(s);
                    }
                }
            }
            "delete" => {
                let total = self.ws.studios.len() + self.ws.assemblies.len() + self.ws.drawings.len();
                if total <= 1 || (matches!(t, Tab::Studio(_)) && self.ws.studios.len() <= 1) {
                    self.flash("A document needs at least one Part Studio", ToastKind::Info);
                    return;
                }
                self.sync_ws();
                self.switch_tab(Tab::Studio(0));
                match t {
                    Tab::Studio(i) => {
                        // move to a studio that stays, then drop this one
                        let keep = if i == 0 { 1 } else { 0 };
                        self.switch_tab(Tab::Studio(keep));
                        self.sync_ws();
                        self.ws.studios.remove(i);
                        let fix = |k: usize| if k > i { k - 1 } else { k };
                        self.cur_studio = fix(self.cur_studio);
                        self.studio_results = std::mem::take(&mut self.studio_results).into_iter().filter(|(k, _)| *k != i).map(|(k, v)| (fix(k), v)).collect();
                        self.studio_undo = std::mem::take(&mut self.studio_undo).into_iter().filter(|(k, _)| *k != i).map(|(k, v)| (fix(k), v)).collect();
                        for a in &mut self.ws.assemblies {
                            a.instances.retain(|x| x.studio != i);
                            for x in &mut a.instances {
                                x.studio = fix(x.studio);
                            }
                        }
                        for d in &mut self.ws.drawings {
                            d.views.retain(|v| v.studio != i);
                            for v in &mut d.views {
                                v.studio = fix(v.studio);
                            }
                        }
                        self.tab = Tab::Studio(self.cur_studio);
                    }
                    Tab::Assembly(i) => {
                        self.ws.assemblies.remove(i);
                    }
                    Tab::Drawing(i) => {
                        self.ws.drawings.remove(i);
                    }
                }
            }
            _ => {}
        }
    }

    fn dock(&mut self, ui: &mut Ui) {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                if let crate::workspace::Tab::Assembly(_) = self.tab {
                    crate::asm_ui::toolbar(self, ui);
                } else if let crate::workspace::Tab::Drawing(_) = self.tab {
                    crate::drawing_ui::toolbar(self, ui);
                } else if self.sketch.is_some() {
                    crate::sketcher::toolbar(self, ui);
                } else {
                    self.model_tools(ui);
                    self.view_toggles(ui);
                    Self::group(ui, |ui| {
                        ui.menu_image_button(icons::image("menu", 18.0).tint(theme::text()), |ui| self.document_menu(ui));
                    });
                }
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_screenshot(&ctx);
        self.pump_kernel();
        self.shortcuts(&ctx);

        let header = Frame::new().fill(theme::header()).inner_margin(Margin::symmetric(8, 3));
        egui::Panel::top("header").frame(header).resizable(false).show(ui, |ui| self.header(ui));
        let bar = Frame::new().fill(theme::panel()).inner_margin(Margin::symmetric(6, 3)).stroke(Stroke::new(1.0, theme::border()));
        egui::Panel::top("toolbar").frame(bar).resizable(false).show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("tabs")
            .frame(Frame::new().fill(theme::tabbar()).inner_margin(Margin { left: 0, right: 6, top: 0, bottom: 0 }))
            .resizable(false)
            .show(ui, |ui| self.tabs_ui(ui));
        if self.phone {
            egui::Panel::bottom("dock").frame(Frame::new().fill(theme::panel()).inner_margin(Margin::same(4))).resizable(false).show(ui, |ui| self.dock(ui));
        }
        if !self.phone && matches!(self.tab, crate::workspace::Tab::Studio(_)) {
            let rail = Frame::new().fill(theme::header()).stroke(Stroke::new(1.0, theme::border()));
            egui::Panel::left("rail").frame(rail).resizable(false).exact_size(40.0).show(ui, |ui| self.rail(ui));
        }
        if self.sidebar_open {
            let side = Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0, theme::border()));
            egui::Panel::left("sidebar").frame(side).resizable(true).default_size(250.0).min_size(190.0).show(ui, |ui| match self.tab {
                crate::workspace::Tab::Assembly(_) => crate::asm_ui::sidebar(self, ui),
                crate::workspace::Tab::Drawing(_) => crate::drawing_ui::sidebar(self, ui),
                _ => self.sidebar(ui),
            });
        }
        let central = egui::CentralPanel::no_frame().show(ui, |ui| match self.tab {
            crate::workspace::Tab::Assembly(_) => crate::asm_ui::viewport(self, ui),
            crate::workspace::Tab::Drawing(_) => crate::drawing_ui::show(self, ui),
            _ => {
                crate::viewport::show(self, ui);
                if self.dialog.is_some() {
                    crate::dialog::show(self, ui);
                }
            }
        });
        if !self.phone && !matches!(self.tab, crate::workspace::Tab::Drawing(_)) {
            self.hint_ui(&ctx, central.response.rect);
        }
        self.toast_ui(&ctx);
        let title = format!("cad-app · {}", self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Sketcher".into()));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}

// ---------------------------------------------------------------- widgets

/// Modifiers held with this frame's key presses (more reliable than the
/// end-of-frame state when press and release arrive together).
pub fn key_mods(ctx: &egui::Context) -> (bool, bool) {
    ctx.input(|i| {
        let mut m = i.modifiers;
        for e in &i.events {
            if let egui::Event::Key { pressed: true, modifiers, .. } = e {
                m = m | *modifiers;
            }
        }
        (m.command, m.shift)
    })
}

fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    Frame::new().inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title.to_uppercase()).color(theme::muted()).size(11.0).strong());
        ui.add_space(4.0);
        add(ui);
    });
    let r = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, theme::border()));
}

#[allow(clippy::too_many_arguments)]
fn tree_row(ui: &mut Ui, indent: u8, icon: &str, name: &str, picked: bool, rolled: bool, suppressed: bool, hidden: bool, error: Option<&str>) -> egui::Response {
    let h = 24.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, CornerRadius::same(3), theme::sel_fill());
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(3), theme::panel_2());
    }
    let alpha = if rolled { 0.4 } else { 1.0 };
    let col = |c: Color32| c.gamma_multiply(alpha);
    let x0 = rect.min.x + 6.0 + indent as f32 * 18.0;
    let icon_rect = egui::Rect::from_min_size(egui::pos2(x0, rect.center().y - 8.0), egui::vec2(16.0, 16.0));
    let icol = if error.is_some() {
        theme::error_soft()
    } else if hidden {
        theme::muted()
    } else {
        theme::text().gamma_multiply(0.85)
    };
    icons::image(icon, 16.0).tint(col(icol)).paint_at(ui, icon_rect);
    let mut color = if error.is_some() { theme::error_soft() } else { theme::text() };
    if suppressed || hidden {
        color = color.gamma_multiply(0.6);
    }
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(name.to_string(), font, col(color));
    let tp = egui::pos2(x0 + 22.0, rect.center().y - galley.size().y / 2.0);
    if suppressed {
        let y = rect.center().y;
        ui.painter().hline(egui::Rangef::new(tp.x, tp.x + galley.size().x), y, Stroke::new(1.0, col(color)));
    }
    ui.painter().galley(tp, galley, col(color));
    if hidden {
        icons::image("eyeOff", 14.0).tint(theme::muted()).paint_at(ui, egui::Rect::from_center_size(egui::pos2(rect.right() - 12.0, rect.center().y), egui::vec2(14.0, 14.0)));
    }
    if error.is_some() {
        ui.painter().text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, "!", egui::FontId::proportional(13.0), theme::error_soft());
    }
    resp
}

/// A document tab in the bottom bar: icon and name, the active one raised.
fn tab_button(ui: &mut Ui, icon: &str, name: &str, active: bool) -> egui::Response {
    let font = egui::FontId::proportional(12.5);
    let col = if active { theme::text() } else { theme::text().gamma_multiply(0.8) };
    let g = ui.painter().layout_no_wrap(name.to_string(), font, col);
    let w = (g.size().x + 44.0).max(120.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 30.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, 0.0, theme::panel());
        p.hline(rect.x_range(), rect.top() + 1.0, Stroke::new(2.0, theme::accent()));
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, theme::panel_2());
    }
    p.vline(rect.right(), rect.y_range().shrink(6.0), Stroke::new(1.0, theme::border()));
    let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(15.0, 15.0));
    icons::image(icon, 15.0).tint(if active { theme::accent() } else { col }).paint_at(ui, ir);
    ui.painter().galley(egui::pos2(rect.left() + 32.0, rect.center().y - g.size().y / 2.0), g, col);
    resp
}

/// A collapsible group header row with a chevron ("Default geometry", "Parts (n)").
fn disclosure_row(ui: &mut Ui, label: &str, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(3), theme::panel_2());
    }
    let ir = egui::Rect::from_center_size(egui::pos2(rect.min.x + 12.0, rect.center().y), egui::vec2(14.0, 14.0));
    icons::image(if open { "chevronDown" } else { "chevronRight" }, 14.0).tint(theme::text()).paint_at(ui, ir);
    ui.painter().text(egui::pos2(rect.min.x + 24.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(13.0), theme::text());
    resp
}

/// Onshape-style rollback bar: a thick grey bar under the last active feature.
fn rollbar(ui: &mut Ui, rolled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), Sense::click());
    let bar = egui::Rect::from_min_max(egui::pos2(rect.left() + 2.0, rect.center().y - 3.0), egui::pos2(rect.right() - 4.0, rect.center().y + 3.0));
    let c = if rolled {
        theme::accent()
    } else if resp.hovered() {
        theme::muted()
    } else {
        theme::rollbar()
    };
    ui.painter().rect_filled(bar, CornerRadius::same(1), c);
    resp.on_hover_text(if rolled { "Rollback bar: click to roll to the end" } else { "Rollback bar: new features are inserted here" })
}

fn is_ident(s: &str) -> bool {
    let s = s.trim();
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch.is_alphabetic() || ch == '_')
        && c.all(|ch| ch.is_alphanumeric() || ch == '_')
        && !["pi", "e", "mm", "cm", "m", "in", "ft", "deg", "rad"].contains(&s)
}

pub fn fmt_volume(mm3: f64, u: Units) -> String {
    format!("{} {}³", crate::expr::trim(mm3 / u.scale().powi(3), 3), u.label())
}

pub fn fmt_area(mm2: f64, u: Units) -> String {
    format!("{} {}²", crate::expr::trim(mm2 / u.scale().powi(2), 3), u.label())
}

/// Light or dark: `CAD_THEME=dark|light` wins, then the saved choice; light by default.
fn load_dark_pref() -> bool {
    if let Ok(t) = std::env::var("CAD_THEME") {
        return t.eq_ignore_ascii_case("dark");
    }
    pref_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.trim() == "dark").unwrap_or(false)
}

fn save_dark_pref(dark: bool) {
    if let Some(p) = pref_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, if dark { "dark" } else { "light" });
    }
}

fn pref_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("cad-rust-app").join("theme"))
}

#[allow(dead_code)]
pub fn feature_icon(f: &Feature) -> &'static str {
    match f.op {
        Op::Sketch { .. } => "sketch",
        _ => "extrude",
    }
}

#[allow(dead_code)]
fn unused(_: HashSet<u8>) {}
