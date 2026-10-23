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
    &["linearPattern", "circularPattern", "curvePattern", "pointPattern", "mirror", "boolean", "split"],
    &["thicken", "rib", "helix", "transform", "moveFace", "deleteFace"],
    &["surfaceExtrude", "surfaceRevolve", "surfaceLoft", "fill", "offsetSurface", "knit"],
    &["sheetMetal", "flange", "tab", "convertSheet", "derived", "heal"],
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
    pub cam: crate::cam_ui::CamUi,
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
    pub script_editor: Option<crate::script_ui::ScriptEditor>,
    /// Sketch button pressed with no plane selected: the next planar pick starts the sketch.
    pub awaiting_sketch_plane: bool,
    pub was_narrow: Option<bool>,
    /// Model-check problem being pointed at in the viewport.
    pub issue_focus: Option<[f64; 3]>,
    pub print: crate::printing::PrintSettings,
    /// Printability colouring (overhangs, thin walls) in the viewport.
    pub print_view: bool,
    /// Per-body triangle classes for the printability view.
    pub print_cache: std::cell::RefCell<std::collections::HashMap<crate::doc::BodyId, Vec<u8>>>,
    /// An STL export on its way to the slicer: (temp path, name).
    pub pending_send: Option<(String, String)>,
    /// Result of the upload, filled in by the upload thread.
    pub send_result: std::sync::Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// Studio lighting with reflections, for presentation.
    pub realistic: bool,
    /// Decoded decal images by (id, data hash): RGBA pixels and size.
    pub decal_images: std::cell::RefCell<std::collections::HashMap<(u64, u64), (Arc<Vec<u8>>, [u32; 2])>>,
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
        theme::install_fonts(&cc.egui_ctx);
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
            cam: Default::default(),
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
            script_editor: None,
            awaiting_sketch_plane: false,
            was_narrow: None,
            issue_focus: None,
            print: crate::printing::load(),
            print_view: false,
            print_cache: Default::default(),
            pending_send: None,
            send_result: Default::default(),
            realistic: false,
            decal_images: Default::default(),
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
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::undo(self);
        }
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
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::redo(self);
        }
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
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::can_undo(self);
        }
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return !self.asm.undo.is_empty();
        }
        !self.undo.is_empty() || self.sketch.as_ref().map(|s| !s.undo.is_empty()).unwrap_or(false)
    }

    pub fn can_redo(&self) -> bool {
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::can_redo(self);
        }
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return !self.asm.redo.is_empty();
        }
        !self.redo.is_empty() || self.sketch.as_ref().map(|s| !s.redo.is_empty()).unwrap_or(false)
    }

    pub fn kernel_ready(&self) -> bool {
        matches!(self.kernel.state, KernelState::Ready | KernelState::Busy)
    }

    /// Start a feature from the toolbar.
    /// A new feature that turned sketches into geometry hides them (show them again from the list).
    fn hide_used_sketches(&mut self, fid: FeatId) {
        let Some(f) = self.doc.feature(fid) else { return };
        if matches!(f.op, Op::Sketch { .. } | Op::Plane { .. }) {
            return;
        }
        fn walk(v: &serde_json::Value, out: &mut Vec<u64>) {
            match v {
                serde_json::Value::Object(m) => {
                    let is_sketch_ref = matches!(m.get("kind").and_then(|k| k.as_str()), Some("region" | "sketch_curve" | "sketch_point"));
                    if is_sketch_ref {
                        if let Some(id) = m.get("sketch").and_then(|s| s.as_u64()) {
                            out.push(id);
                        }
                    }
                    m.values().for_each(|x| walk(x, out));
                }
                serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut ids = vec![];
        walk(&serde_json::to_value(&f.op).unwrap_or_default(), &mut ids);
        let mut any = false;
        for id in ids {
            if let Some(sk) = self.doc.feature_mut(id) {
                if matches!(sk.op, Op::Sketch { .. }) && !sk.hidden {
                    sk.hidden = true;
                    any = true;
                }
            }
        }
        if any {
            self.scene_dirty = true;
        }
    }

    /// Finish the open sketch and start an Extrude of its regions (holes stay empty).
    pub fn extrude_from_sketch(&mut self) {
        let Some(fid) = self.sketch.as_ref().map(|s| s.feature) else { return };
        crate::sketcher::finish(self);
        let Some(Op::Sketch { sketch, .. }) = self.doc.feature(fid).map(|f| f.op.clone()) else { return };
        let regions = crate::regions::find_regions(&sketch);
        if regions.is_empty() {
            self.flash("The sketch has no closed regions to extrude", ToastKind::Info);
            return;
        }
        self.picks = regions
            .iter()
            .filter(|r| !regions.iter().any(|o| o.holes.iter().any(|h| crate::regions::point_in_poly(r.sample, &h.poly))))
            .map(|r| Ref::Region { sketch: fid, point: r.sample.into() })
            .collect();
        self.start_feature("extrude");
    }

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
            } else if d.is_new {
                self.hide_used_sketches(d.feature);
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
        self.cam = Default::default();
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
        if matches!(t, Tab::Assembly(_) | Tab::Drawing(_) | Tab::Cam(_)) {
            // every Part Studio needs geometry for the assembly/drawing/manufacture tab
            for i in 0..self.ws.studios.len() {
                if i != self.cur_studio && !self.studio_results.contains_key(&i) {
                    let d = self.ws.studios[i].doc.clone();
                    let docs = self.studio_docs();
                    self.kernel.set_studios(docs);
                    self.kernel.rebuild(i, &d);
                }
            }
            self.asm.scene_dirty = true;
            self.dwg.dirty = true;
            self.cam.scene_dirty = true;
        }
        self.picks.clear();
        self.hover = None;
        self.tab = t;
        self.scene_dirty = true;
    }

    /// Every Part Studio's document, with the live one in place.
    pub fn studio_docs(&self) -> Vec<Document> {
        self.ws.studios.iter().enumerate().map(|(i, s)| if i == self.cur_studio { self.doc.clone() } else { s.doc.clone() }).collect()
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
        let exts = ["step", "stp", "igs", "iges", "stl", "obj", "dxf", "svg", "STEP", "STP", "IGS", "IGES", "STL", "OBJ", "DXF", "SVG"];
        let Some(p) = rfd::FileDialog::new().add_filter("STEP, IGES, STL, OBJ, DXF, SVG", &exts).pick_file() else { return };
        self.import_path(&p);
    }

    pub fn import_path(&mut self, p: &std::path::Path) {
        if crate::sketch_import::is_drawing(p) {
            return self.import_drawing(p);
        }
        let Some(format) = crate::import::format_of(p) else {
            self.flash("Import STEP, IGES, STL, OBJ, DXF or SVG files", ToastKind::Info);
            return;
        };
        match std::fs::read(p) {
            Ok(bytes) => {
                self.checkpoint();
                let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Import".into());
                let op = Op::Import { name: name.clone(), step: String::new(), format: format.into(), data: crate::import::pack(&bytes), scale: "1".into(), merge: true };
                self.doc.insert(name, op);
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
        let format = if format == "stl" { format!("stl@{}", self.print.stl_tolerance) } else { format.to_string() };
        self.kernel.export(&self.doc, &format, p.to_string_lossy().to_string());
        self.flash_info(format!("Exporting {}…", p.display()), None);
    }

    /// Make an STL of the parts and hand it to orcaweb.
    pub fn send_to_slicer(&mut self) {
        if self.result.bodies.is_empty() {
            self.flash("There are no parts to print", ToastKind::Info);
            return;
        }
        let name = self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "part".into());
        let tmp = std::env::temp_dir().join(format!("cad-app-send-{}.stl", std::process::id()));
        let path = tmp.to_string_lossy().to_string();
        self.pending_send = Some((path.clone(), name));
        self.kernel.export(&self.doc, &format!("stl@{}", self.print.stl_tolerance), path);
        self.flash_info("Sending to the slicer…", None);
    }

    fn print_settings_ui(&mut self, ui: &mut Ui) {
        ui.set_min_width(300.0);
        let before = self.print.clone();
        let p = &mut self.print;
        egui::Grid::new("print-settings").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Slicer (orcaweb)");
            ui.add(egui::TextEdit::singleline(&mut p.orcaweb).desired_width(190.0));
            ui.end_row();
            ui.label("STL detail");
            let cur = match p.stl_tolerance {
                t if t >= 0.04 => "Coarse",
                t if t >= 0.008 => "Normal",
                _ => "Fine",
            };
            egui::ComboBox::from_id_salt("stl-detail").selected_text(cur).show_ui(ui, |ui| {
                for (t, l) in [(0.05, "Coarse (smaller files)"), (0.01, "Normal"), (0.002, "Fine (smooth curves)")] {
                    if ui.selectable_label(cur == l.split(' ').next().unwrap(), l).clicked() {
                        p.stl_tolerance = t;
                    }
                }
            });
            ui.end_row();
            ui.label("Overhang limit");
            ui.add(egui::DragValue::new(&mut p.overhang).range(10.0..=80.0).suffix("°"));
            ui.end_row();
            ui.label("Thinnest wall");
            ui.add(egui::DragValue::new(&mut p.min_wall).range(0.0..=10.0).speed(0.05).suffix(" mm"));
            ui.end_row();
        });
        if self.print != before {
            crate::printing::save(&self.print);
            self.print_cache.borrow_mut().clear();
            self.scene_dirty = true;
        }
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
                    // Part Studios deriving from this one need rebuilding too
                    let cur = self.cur_studio;
                    for i in 0..self.ws.studios.len() {
                        let derives = i != cur && self.ws.studios[i].doc.features.iter().any(|f| matches!(f.op, Op::Derived { studio, .. } if studio == cur));
                        if derives {
                            self.studio_results.remove(&i);
                            let d = self.ws.studios[i].doc.clone();
                            let docs = self.studio_docs();
                            self.kernel.set_studios(docs);
                            self.kernel.rebuild(i, &d);
                        }
                    }
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
                    self.print_cache.borrow_mut().clear();
                    self.scene_dirty = true;
                    if self.first_fit && !self.result.bodies.is_empty() {
                        self.first_fit = false;
                        self.fit_view();
                    }
                }
                Outcome::Interference(pairs) => crate::asm_ui::interference_result(self, pairs),
                Outcome::Projected(tag, key, views) => crate::drawing_ui::projected(self, tag, key, views),
                Outcome::Exported(err, path) if self.pending_send.as_ref().map(|p| p.0 == path).unwrap_or(false) => {
                    let (_, name) = self.pending_send.take().unwrap();
                    match err {
                        Some(e) => self.flash(format!("Could not make the STL: {e}"), ToastKind::Error),
                        None => {
                            let bytes = std::fs::read(&path).unwrap_or_default();
                            let _ = std::fs::remove_file(&path);
                            let url = self.print.orcaweb.clone();
                            let slot = self.send_result.clone();
                            std::thread::spawn(move || {
                                let r = crate::printing::send_to_orcaweb(&url, &name, &bytes);
                                *slot.lock().unwrap() = Some(r);
                            });
                        }
                    }
                }
                Outcome::Exported(err, path) => match err {
                    None => self.flash_info(format!("Exported {path}"), Some(Duration::from_secs(3))),
                    Some(e) => self.flash(format!("Export failed: {e}"), ToastKind::Error),
                },
            }
        }
        let sent = self.send_result.lock().ok().and_then(|mut s| s.take());
        match sent {
            Some(Ok(url)) => {
                crate::printing::open_url(&url);
                self.flash_info("Sent to the slicer · its page opens in your browser", Some(Duration::from_secs(4)));
            }
            Some(Err(e)) => self.flash(format!("Could not send to the slicer: {e}"), ToastKind::Error),
            None => {}
        }
        if was_loading && self.kernel_ready() {
            if self.toast.as_ref().map(|t| t.text.starts_with("Loading")).unwrap_or(false) {
                self.toast = None;
            }
        }
        if self.dirty_model && self.kernel.state != KernelState::Error {
            self.dirty_model = false;
            let cur = self.cur_studio;
            let docs = self.studio_docs();
            self.kernel.set_studios(docs);
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
        for b in self.result.bodies.iter().filter(|b| !self.part_hidden(b.id)) {
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
            crate::workspace::Tab::Cam(_) => return crate::cam_ui::shortcuts(self, ctx),
            _ => {}
        }
        if self.sketch.is_some() {
            crate::sketcher::shortcuts(self, ctx);
            return;
        }
        if pressed(Key::Escape) {
            if self.awaiting_sketch_plane {
                self.awaiting_sketch_plane = false;
                self.toast = None;
            } else if self.dialog.is_some() {
                self.dialog_cancel();
            } else if self.measure.is_some() {
                self.measure = None;
            } else {
                self.picks.clear();
                self.issue_focus = None;
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
        if pressed(Key::N) {
            self.view_normal_to_selection();
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
        ui.painter().vline(rect.center().x, rect.y_range().shrink(5.0), Stroke::new(1.0, theme::border()));
        ui.add_space(3.0);
        ui.spacing_mut().item_spacing.x = 1.0;
        add(ui);
        ui.spacing_mut().item_spacing.x = 1.0;
    }

    /// Top strip: logo, document menu and name, theme and phone toggles.
    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add(icons::image("logo", 20.0).tint(theme::accent()));
            if !self.phone {
                ui.label(RichText::new("cad-app").family(theme::medium()).size(15.0).color(theme::text()));
                let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 18.0), Sense::hover());
                ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, theme::border()));
            }
            ui.menu_image_button(icons::image("menu", 18.0).tint(theme::text()), |ui| self.document_menu(ui)).response.on_hover_text("Document menu");
            let name = self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled document".into());
            let name = if self.phone && name.chars().count() > 18 { format!("{}…", name.chars().take(17).collect::<String>()) } else { name };
            ui.label(RichText::new(name).family(theme::medium()).size(14.0).color(theme::text()));
            if !self.phone {
                // branch badge
                Frame::new().fill(theme::panel_2()).corner_radius(4).inner_margin(Margin::symmetric(6, 1)).show(ui, |ui| {
                    ui.label(RichText::new("Main").size(11.0).color(theme::muted()));
                });
            }
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
                ui.add_space(4.0);
                if !self.phone {
                    ui.label(RichText::new(txt).size(11.5).color(theme::muted()));
                }
                let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.5, col.gamma_multiply(0.25));
                ui.painter().circle_filled(r.center(), 3.5, col);
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
        for (a, icon, l) in [("new", "file", "New part"), ("open", "file", "Open .json…"), ("json", "download", "Save .json (Ctrl+S)"), ("import", "import", "Import STEP, IGES, STL, OBJ…")]
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
        if ui.add(egui::Button::image_and_text(icons::image("printer", 15.0).tint(theme::text()), "Send to slicer")).on_hover_text(format!("Open the parts in orcaweb ({}) to slice and print", self.print.orcaweb)).clicked() {
            action = Some("send");
            ui.close();
        }
        ui.menu_image_text_button(icons::image("printer", 15.0).tint(theme::text()), "3D printing settings", |ui| self.print_settings_ui(ui));
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
            Some("send") => self.send_to_slicer(),
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
                    crate::workspace::Tab::Cam(_) => crate::cam_ui::toolbar(self, ui),
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
                    let ex = egui::Button::image_and_text(icons::image("extrude", 18.0).tint(theme::text()), RichText::new("Extrude").size(12.5)).min_size(egui::vec2(26.0, 26.0));
                    if ui.add(ex).on_hover_text("Finish the sketch and extrude its regions (Shift+E)").clicked() {
                        self.extrude_from_sketch();
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
            egui::TextEdit::singleline(&mut self.tool_search).hint_text(RichText::new("Search tools…").color(theme::muted())).desired_width(170.0).margin(egui::vec2(8.0, 5.0)),
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
        crate::script_ui::toolbar(self, ui, ready);
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
                let mut b = egui::Button::image(icons::image(icon, 18.0).tint(fg)).min_size(egui::vec2(30.0, 30.0));
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
                let parts_h = if self.parts_open { (28.0 + 26.0 * self.result.bodies.len().max(1) as f32 + 14.0).min(total * 0.4) } else { 30.0 };
                let props = self.picks.iter().any(|r| matches!(r, Ref::Body { .. }));
                let props_h = if props { 190.0f32.min(total * 0.35) } else { 0.0 };
                let issues_h = if self.result.issues.is_empty() { 0.0 } else { (90.0 + 34.0 * self.result.issues.len().min(4) as f32).min(total * 0.35) };
                let decals_h = if self.doc.decals.is_empty() { 0.0 } else { (40.0 + 52.0 * self.doc.decals.len().min(3) as f32).min(total * 0.25) };
                let top_h = (total - parts_h - props_h - issues_h - decals_h).max(80.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), top_h), |ui| {
                    egui::ScrollArea::vertical().id_salt("feature-scroll").auto_shrink([false, false]).show(ui, |ui| {
                        Frame::new().inner_margin(Margin { left: 8, right: 8, top: 8, bottom: 6 }).show(ui, |ui| self.feature_tree(ui));
                    });
                });
                let y = ui.cursor().top();
                ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, theme::border()));
                Frame::new().inner_margin(Margin { left: 8, right: 8, top: 4, bottom: 2 }).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("parts-scroll").max_height(parts_h - 6.0).auto_shrink([false, true]).show(ui, |ui| self.parts_list(ui));
                });
                if !self.doc.decals.is_empty() {
                    egui::ScrollArea::vertical().id_salt("decals-scroll").max_height(decals_h).auto_shrink([false, true]).show(ui, |ui| {
                        section(ui, "Decals", |ui| self.decals_ui(ui));
                    });
                }
                if !self.result.issues.is_empty() {
                    egui::ScrollArea::vertical().id_salt("issues-scroll").max_height(issues_h).auto_shrink([false, true]).show(ui, |ui| {
                        section(ui, "Model check", |ui| self.issues_ui(ui));
                    });
                }
                if props {
                    // fits in what is left above the tab bar, scrolling if the panel is short
                    egui::ScrollArea::vertical().id_salt("props-scroll").max_height(ui.available_height()).auto_shrink([false, true]).show(ui, |ui| {
                        section(ui, "Mass properties", |ui| self.properties(ui));
                    });
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
                    .hint_text(RichText::new("Filter by name or type").color(theme::muted()))
                    .desired_width(ui.available_width()),
            );
        });
        ui.add_space(6.0);
        // header
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Features ({})", self.doc.features.len())).family(theme::medium()).size(12.5));
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
                let mut toggle: Option<String> = None;
                let hid = self.doc.hidden_defaults.iter().any(|h| h == "Origin");
                let resp = tree_row(ui, 1, "origin", "Origin", false, false, false, hid, None).on_hover_text("Origin (0, 0, 0)");
                if eye_toggle(ui, &resp, resp.rect.right() - 12.0, hid, "eye-origin") {
                    toggle = Some("Origin".into());
                }
                resp.context_menu(|ui| {
                    if ui.button(if hid { "Show" } else { "Hide" }).clicked() {
                        toggle = Some("Origin".into());
                        ui.close();
                    }
                });
                for p in OriginPlane::ALL {
                    let r = Ref::OriginPlane { plane: p };
                    let picked = self.picks.contains(&r);
                    let label = p.name().trim_end_matches(" plane");
                    let hid = self.doc.hidden_defaults.iter().any(|h| h == label);
                    let resp = tree_row(ui, 1, "planeSheet", label, picked, false, false, hid, None);
                    if eye_toggle(ui, &resp, resp.rect.right() - 12.0, hid, ("eye-plane", label)) {
                        toggle = Some(label.to_string());
                    }
                    resp.context_menu(|ui| {
                        if ui.button(if hid { "Show" } else { "Hide" }).clicked() {
                            toggle = Some(label.to_string());
                            ui.close();
                        }
                    });
                    if resp.clicked() {
                        self.pick_from_tree(r.clone(), ui.input(|i| i.modifiers.shift || i.modifiers.command));
                    }
                    if resp.hovered() && self.hover.as_ref() != Some(&r) {
                        self.hover = Some(r);
                        self.scene_dirty = true;
                    }
                }
                if let Some(t) = toggle {
                    // display only: no rebuild
                    self.checkpoint();
                    if let Some(i) = self.doc.hidden_defaults.iter().position(|h| *h == t) {
                        self.doc.hidden_defaults.remove(i);
                    } else {
                        self.doc.hidden_defaults.push(t);
                    }
                    self.scene_dirty = true;
                }
            }
        }

        let rb = self.doc.rollback_index();
        let mut action: Option<(&'static str, FeatId)> = None;
        let mut drop_rollback: Option<usize> = None;
        let n = self.doc.features.len();
        // feature row extents, for dragging the rollback bar between them
        let mut rows: Vec<(usize, egui::Rangef)> = vec![];
        let mut bar_resp: Option<egui::Response> = None;
        for i in 0..=n {
            if i == rb {
                let resp = rollbar(ui, self.doc.rollback.is_some());
                if resp.clicked() && self.doc.rollback.is_some() {
                    drop_rollback = Some(usize::MAX);
                }
                bar_resp = Some(resp);
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
            rows.push((i, egui::Rangef::new(ui.cursor().top(), ui.cursor().top() + 24.0)));
            let picked = self.picks.iter().any(|r| matches!(r, Ref::FeaturePlane { feature } if *feature == id));
            let err = self.errors.get(&id).cloned();
            let resp = tree_row(ui, 0, f.op.kind(), &f.name, picked || editing, i >= rb, f.suppressed, f.hidden, err.as_deref());
            if matches!(f.op, Op::Sketch { .. } | Op::Plane { .. } | Op::Helix { .. }) && eye_toggle(ui, &resp, resp.rect.right() - 12.0, f.hidden, ("feat-eye", id)) {
                action = Some(("hide", id));
            }
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
        // drag the bar up or down the list to roll the model back or forward
        if let Some(resp) = bar_resp {
            if resp.dragged() || resp.drag_stopped() {
                if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                    // the bar goes after every row whose middle is above the pointer
                    let at = rows.iter().filter(|(_, r)| r.center() < p.y).map(|(i, _)| i + 1).max().unwrap_or(0);
                    let y = rows.iter().find(|(i, _)| *i + 1 == at).map(|(_, r)| r.max).or_else(|| rows.first().map(|(_, r)| r.min)).unwrap_or(p.y);
                    let x = resp.rect.x_range();
                    ui.painter().hline(x, y, Stroke::new(3.0, theme::accent()));
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    if resp.drag_stopped() && at != rb {
                        drop_rollback = Some(at);
                    }
                }
            } else if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
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

    pub fn set_part_hidden(&mut self, id: doc::BodyId, hidden: bool) {
        self.set_part(id, |p| p.hidden = hidden);
    }

    pub fn show_all_parts(&mut self) {
        if self.doc.parts.values().any(|p| p.hidden) {
            self.checkpoint();
            for p in self.doc.parts.values_mut() {
                p.hidden = false;
            }
            self.doc.parts.retain(|_, p| *p != doc::PartProps::default());
            self.changed();
        }
    }

    /// The sketch frame of a planar selection (plane, flat face, or a sketch's plane).
    pub fn frame_of(&self, r: &Ref) -> Option<crate::sketch::Frame> {
        match r {
            Ref::OriginPlane { plane } => Some(plane.frame()),
            Ref::FeaturePlane { feature } => self.result.frames.get(feature).copied(),
            Ref::Face { point, normal } if r.is_planar() => Some(crate::kernel::plane_frame((*point).into(), (*normal).into())),
            _ => None,
        }
    }

    /// Look straight at the sketch being edited, or at the selected plane or flat face.
    pub fn view_normal_to_selection(&mut self) {
        let target = if let Some(s) = &self.sketch {
            self.doc.feature(s.feature).and_then(|f| if let Op::Sketch { frame, .. } = &f.op { Some((*frame, None)) } else { None })
        } else {
            self.picks.iter().find_map(|r| {
                let at = if let Ref::Face { point, .. } = r { Some(glam::DVec3::from(*point)) } else { None };
                self.frame_of(r).map(|f| (f, at))
            })
        };
        match target {
            Some((fr, at)) => {
                self.camera.set_orientation(fr.normal().as_vec3(), fr.y.map(|v| v as f32).into());
                if let Some(p) = at {
                    self.camera.target = p.as_vec3();
                }
                self.scene_dirty = true;
            }
            None => self.flash("Select a plane or flat face to look at", ToastKind::Info),
        }
    }

    pub fn default_hidden(&self, name: &str) -> bool {
        self.doc.hidden_defaults.iter().any(|h| h == name)
    }

    pub fn part_hidden(&self, id: doc::BodyId) -> bool {
        self.doc.parts.get(&doc::body_key(id)).map(|p| p.hidden).unwrap_or(false)
    }

    pub fn part_props(&self, id: doc::BodyId) -> doc::PartProps {
        self.doc.parts.get(&doc::body_key(id)).cloned().unwrap_or_default()
    }

    pub fn part_name(&self, id: doc::BodyId) -> String {
        let p = self.part_props(id);
        p.name.unwrap_or_else(|| self.result.bodies.iter().find(|b| b.id == id).map(|b| b.name.clone()).unwrap_or_default())
    }

    /// Finish code for the shader: the part's own, else one that suits its material.
    pub fn part_finish(&self, id: doc::BodyId) -> f32 {
        let p = self.part_props(id);
        if p.finish.is_some() {
            return crate::render::finish_code(p.finish.as_deref());
        }
        match p.material.as_deref() {
            Some(m) if ["Aluminium", "Steel", "Stainless", "Brass", "Copper", "Titanium"].iter().any(|k| m.starts_with(k)) => crate::render::finish_code(Some("metal")),
            Some(m) if ["PLA", "PETG"].iter().any(|k| m.starts_with(k)) => crate::render::finish_code(Some("satin")),
            Some(_) => crate::render::finish_code(Some("matte")),
            None if self.realistic => crate::render::finish_code(Some("satin")),
            None => 0.0,
        }
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
            let hidden = self.part_hidden(id);
            let resp = tree_row(ui, 1, "part", &name, picked, false, false, false, None);
            // colour swatch
            let sw = egui::Rect::from_center_size(egui::pos2(resp.rect.right() - 12.0, resp.rect.center().y), egui::vec2(10.0, 10.0));
            ui.painter().rect_filled(sw, 2.0, if hidden { self.part_color(id).gamma_multiply(0.3) } else { self.part_color(id) });
            if eye_toggle(ui, &resp, resp.rect.right() - 32.0, hidden, ("part-eye", id)) {
                self.set_part(id, |p| p.hidden = !p.hidden);
            }
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
                if ui.button(if hidden { "Show" } else { "Hide" }).clicked() {
                    self.set_part(id, |p| p.hidden = !p.hidden);
                    ui.close();
                }
                if ui.button("Show all parts").clicked() {
                    self.show_all_parts();
                    ui.close();
                }
                ui.menu_button("Appearance", |ui| {
                    let mut c = self.part_color(id);
                    if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
                        self.set_part(id, |p| p.color = Some([c.r(), c.g(), c.b()]));
                    }
                    ui.horizontal_wrapped(|ui| {
                        let cur = self.part_props(id).finish;
                        for (key, label, _) in crate::render::FINISHES {
                            let on = cur.as_deref().unwrap_or("") == *key;
                            if ui.selectable_label(on, *label).clicked() {
                                let v = (!key.is_empty()).then(|| key.to_string());
                                self.set_part(id, |p| p.finish = v);
                            }
                        }
                    });
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

    fn issues_ui(&mut self, ui: &mut Ui) {
        let issues = self.result.issues.clone();
        for (i, is) in issues.iter().enumerate() {
            let on = self.issue_focus == Some(is.at);
            let text = format!("{} · {}", is.describe(), self.part_name(is.body));
            let r = ui.add(
                egui::Button::image_and_text(icons::image("warning", 14.0).tint(theme::error_soft()), RichText::new(text).size(12.0))
                    .selected(on)
                    .min_size(egui::vec2(ui.available_width(), 20.0)),
            );
            if r.on_hover_text("Show where it is").clicked() {
                self.focus_issue(i);
            }
        }
        ui.horizontal(|ui| {
            if ui.add(egui::Button::image_and_text(icons::image("heal", 14.0).tint(theme::text()), "Heal parts")).on_hover_text("Add a Heal feature: removes tiny edges and faces and closes small gaps").clicked() {
                self.picks = issues.iter().map(|i| Ref::Body { body: i.body }).collect::<Vec<_>>();
                self.picks.dedup();
                self.start_feature("heal");
            }
        });
    }

    /// Lay an image on a flat face.
    pub fn add_decal(&mut self, at: [f64; 3], normal: [f64; 3]) {
        let Some(p) = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"]).pick_file() else { return };
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) => return self.flash(format!("Could not read {}: {e}", p.display()), ToastKind::Error),
        };
        if image::load_from_memory(&bytes).is_err() {
            return self.flash("That file is not a PNG or JPEG image", ToastKind::Error);
        }
        let (lo, hi) = self.model_bounds();
        let size = if lo.x.is_finite() { (hi - lo).length() } else { 50.0 };
        self.checkpoint();
        let id = self.doc.decals.iter().map(|d| d.id).max().unwrap_or(0) + 1;
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Decal".into());
        self.doc.decals.push(doc::Decal { id, name, image: crate::import::pack(&bytes), at, normal, width: (size * 0.25).max(5.0).round(), angle: 0.0 });
        self.changed();
    }

    fn decals_ui(&mut self, ui: &mut Ui) {
        let mut del = None;
        let mut edits: Vec<(usize, f64, f64)> = vec![];
        for (i, d) in self.doc.decals.iter().enumerate() {
            ui.label(RichText::new(&d.name).size(12.5));
            ui.horizontal(|ui| {
                let (mut w, mut a) = (d.width, d.angle);
                let r1 = ui.add(egui::DragValue::new(&mut w).range(0.1..=10000.0).speed(0.5).prefix("width ").suffix(" mm"));
                let r2 = ui.add(egui::DragValue::new(&mut a).range(-360.0..=360.0).speed(1.0).prefix("turn ").suffix("°"));
                if r1.changed() || r2.changed() {
                    edits.push((i, w, a));
                }
                if ui.add(egui::Button::image(icons::image("close", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Remove").clicked() {
                    del = Some(i);
                }
            });
        }
        if !edits.is_empty() || del.is_some() {
            self.checkpoint();
            for (i, w, a) in edits {
                self.doc.decals[i].width = w;
                self.doc.decals[i].angle = a;
            }
            if let Some(i) = del {
                self.doc.decals.remove(i);
            }
            // decals are display only: no rebuild needed
            self.scene_dirty = true;
        }
    }

    /// Decals as textured quads for the renderer.
    pub fn decal_draws(&self) -> Vec<crate::render::DecalDraw> {
        use std::hash::{Hash, Hasher};
        let mut out = vec![];
        for d in &self.doc.decals {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            d.image.hash(&mut h);
            let key = (d.id, h.finish());
            let mut cache = self.decal_images.borrow_mut();
            if !cache.contains_key(&key) {
                let Ok(img) = crate::import::unpack(&d.image).and_then(|b| image::load_from_memory(&b).map_err(|e| e.to_string())) else { continue };
                // keep textures a sensible size
                let img = if img.width().max(img.height()) > 2048 { img.thumbnail(2048, 2048) } else { img };
                let rgba = img.to_rgba8();
                let size = [rgba.width(), rgba.height()];
                cache.insert(key, (Arc::new(rgba.into_raw()), size));
            }
            let (rgba, size) = cache[&key].clone();
            let n = glam::DVec3::from(d.normal).normalize_or(glam::DVec3::Z);
            let fr = crate::kernel::frame_from(glam::DVec3::from(d.at), n);
            let rot = glam::DMat3::from_axis_angle(n, d.angle.to_radians());
            let (x, y) = (rot * glam::DVec3::from(fr.x), rot * glam::DVec3::from(fr.y));
            let (w, hgt) = (d.width, d.width * size[1] as f64 / size[0].max(1) as f64);
            let c = glam::DVec3::from(d.at) + n * 0.01;
            let corner = |sx: f64, sy: f64| (c + x * (sx * w / 2.0) + y * (sy * hgt / 2.0)).as_vec3().to_array();
            let k = key.0.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ key.1;
            out.push(crate::render::DecalDraw { key: k, rgba, size, corners: [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)] });
        }
        out
    }

    /// Point the camera at a model-check problem and mark it.
    pub fn focus_issue(&mut self, i: usize) {
        let Some(is) = self.result.issues.get(i) else { return };
        let p = glam::DVec3::from(is.at);
        self.camera.target = p.as_vec3();
        let (lo, hi) = self.model_bounds();
        let size = if lo.x.is_finite() { (hi - lo).length() } else { 50.0 };
        self.camera.half_height = ((size * 0.08) as f32).max(1.0);
        self.issue_focus = Some(is.at);
        self.scene_dirty = true;
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
        // label and value on one line, the value wrapping under it when the panel is narrow
        let row = |ui: &mut Ui, k: &str, v: String| {
            ui.horizontal_wrapped(|ui| {
                ui.add_sized([96.0, 16.0], egui::Label::new(RichText::new(k).color(theme::muted()).size(12.0)));
                ui.add(egui::Label::new(RichText::new(v).size(12.0)).wrap());
            });
        };
        ui.spacing_mut().item_spacing.y = 3.0;
        row(ui, "Volume", fmt_volume(vol, units));
        row(ui, "Surface area", fmt_area(area, units));
        row(ui, "Mass", if all_mass { format!("{} g", crate::expr::trim(mass, 2)) } else { "set a material".into() });
        row(ui, "Centre of mass", format!("{}, {}, {}", units.fmt(c.x), units.fmt(c.y), units.fmt(c.z)));
        row(ui, "Size", format!("{} × {} × {} {}", units.fmt(size.x), units.fmt(size.y), units.fmt(size.z), units.label()));
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
        } else if let crate::workspace::Tab::Cam(_) = self.tab {
            "Set up the stock, add tools and operations, simulate, then export G-code · Space plays the simulation".to_string()
        } else if let Some(s) = &self.sketch {
            crate::sketcher::status_text(self, s)
        } else if self.print_view && self.sketch.is_none() && self.dialog.is_none() {
            format!("Printability · red: overhang past {}° needs support · purple: wall thinner than {} mm", self.print.overhang, crate::expr::trim(self.print.min_wall, 2))
        } else if self.awaiting_sketch_plane {
            "Select a plane or flat face to sketch on · Esc cancels".to_string()
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
        let at = egui::pos2(area.left() + 16.0, area.bottom() - 14.0 - g.size().y);
        let pill = egui::Rect::from_min_size(at, g.size()).expand2(egui::vec2(10.0, 5.0));
        painter.rect(pill, 6.0, theme::label_bg(), Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
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
                Frame::new().fill(fill).stroke(Stroke::new(1.0, stroke)).corner_radius(8).inner_margin(Margin::symmetric(14, 8)).shadow(ui.visuals().popup_shadow).show(ui, |ui| {
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
        for (i, c) in self.ws.cams.iter().enumerate() {
            tabs.push((Tab::Cam(i), c.name.clone(), "cam"));
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
                if ui.add(egui::Button::image_and_text(icons::image("cam", 15.0).tint(theme::text()), "Manufacture")).clicked() {
                    action = Some(("new-cam", Tab::Studio(0)));
                    ui.close();
                }
            })
            .response
            .on_hover_text("Add a Part Studio, Assembly, Drawing or Manufacture tab");
            ui.add_space(6.0);
            // tabs scroll sideways when they don't fit (phones)
            let reserve = if self.phone { 115.0 } else { 220.0 };
            egui::ScrollArea::horizontal().max_width((ui.available_width() - reserve).max(80.0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
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
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_space(4.0);
                self.units_combo(ui);
                let versions = if self.phone { format!("V{}", self.ws.versions.len()) } else { format!("Versions ({})", self.ws.versions.len()) };
                ui.menu_button(RichText::new(versions).size(12.0), |ui| {
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
            Tab::Cam(i) => ws.cams.get(i).map(|s| s.name.clone()),
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
                            Tab::Cam(i) => {
                                // setups that carry on from this one follow the new name
                                let old = std::mem::replace(&mut self.ws.cams[i].name, text.clone());
                                for c in self.ws.cams.iter_mut().filter(|c| c.follows == old) {
                                    c.follows = text.clone();
                                }
                            }
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
            "new-cam" => {
                let n = self.ws.cams.len() + 1;
                let studio = match self.tab {
                    Tab::Studio(i) => i,
                    _ => self.cur_studio,
                };
                self.ws.cams.push(crate::cam::Cam { name: format!("Manufacture {n}"), studio, ..Default::default() });
                self.cam = Default::default();
                self.switch_tab(Tab::Cam(n - 1));
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
                    Tab::Cam(i) => {
                        let mut s = self.ws.cams[i].clone();
                        s.name = format!("{} copy", s.name);
                        self.ws.cams.push(s);
                    }
                }
            }
            "delete" => {
                let total = self.ws.studios.len() + self.ws.assemblies.len() + self.ws.drawings.len() + self.ws.cams.len();
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
                            a.instances.retain(|x| x.sub.is_some() || x.studio != i);
                            for x in a.instances.iter_mut().filter(|x| x.sub.is_none()) {
                                x.studio = fix(x.studio);
                            }
                        }
                        for d in &mut self.ws.drawings {
                            d.views.retain(|v| v.studio != i);
                            for v in &mut d.views {
                                v.studio = fix(v.studio);
                            }
                        }
                        for c in &mut self.ws.cams {
                            if c.studio == i {
                                c.studio = 0;
                                c.bodies.clear();
                            } else {
                                c.studio = fix(c.studio);
                            }
                        }
                        self.tab = Tab::Studio(self.cur_studio);
                    }
                    Tab::Assembly(i) => {
                        self.ws.assemblies.remove(i);
                        // drop instances of it in other assemblies and renumber the rest
                        for a in &mut self.ws.assemblies {
                            a.instances.retain(|x| x.sub != Some(i));
                            for x in &mut a.instances {
                                x.sub = x.sub.map(|k| if k > i { k - 1 } else { k });
                            }
                        }
                    }
                    Tab::Drawing(i) => {
                        self.ws.drawings.remove(i);
                    }
                    Tab::Cam(i) => {
                        self.ws.cams.remove(i);
                        self.cam = Default::default();
                    }
                }
            }
            _ => {}
        }
    }

    /// Phone layout: the sidebar slides over the viewport from the left; tapping
    /// beside it closes it.
    fn drawer(&mut self, ctx: &egui::Context, area: egui::Rect) {
        let w = (area.width() * 0.86).min(340.0);
        let shade = egui::Area::new(egui::Id::new("drawer-shade")).order(egui::Order::Middle).fixed_pos(area.min).show(ctx, |ui| {
            let (r, resp) = ui.allocate_exact_size(area.size(), Sense::click());
            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(70));
            resp
        });
        if shade.inner.clicked() {
            self.sidebar_open = false;
            return;
        }
        egui::Area::new(egui::Id::new("drawer")).order(egui::Order::Foreground).fixed_pos(area.min).show(ctx, |ui| {
            Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0, theme::border())).show(ui, |ui| {
                ui.set_width(w);
                ui.set_height(area.height());
                if matches!(self.tab, crate::workspace::Tab::Studio(_)) {
                    // the rail's panes, as tabs
                    ui.horizontal(|ui| {
                        for (pane, icon, label) in [(LeftPane::Features, "features", "Features"), (LeftPane::Variables, "variable", "Variables"), (LeftPane::Configurations, "configurations", "Configs")] {
                            let on = self.left_pane == pane;
                            let fg = if on { theme::active_text() } else { theme::text() };
                            let mut b = egui::Button::image_and_text(icons::image(icon, 16.0).tint(fg), RichText::new(label).size(12.5).color(fg)).min_size(egui::vec2(0.0, 32.0));
                            if on {
                                b = b.fill(theme::active_fill());
                            }
                            if ui.add(b).clicked() {
                                self.left_pane = pane;
                            }
                        }
                    });
                    ui.separator();
                }
                match self.tab {
                    crate::workspace::Tab::Assembly(_) => crate::asm_ui::sidebar(self, ui),
                    crate::workspace::Tab::Drawing(_) => crate::drawing_ui::sidebar(self, ui),
                    crate::workspace::Tab::Cam(_) => crate::cam_ui::sidebar(self, ui),
                    _ => self.sidebar(ui),
                }
            });
        });
    }

    fn dock(&mut self, ui: &mut Ui) {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                if let crate::workspace::Tab::Assembly(_) = self.tab {
                    crate::asm_ui::toolbar(self, ui);
                } else if let crate::workspace::Tab::Drawing(_) = self.tab {
                    crate::drawing_ui::toolbar(self, ui);
                } else if let crate::workspace::Tab::Cam(_) = self.tab {
                    crate::cam_ui::toolbar(self, ui);
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
        // narrow windows (phones) get the phone layout; crossing back restores the desktop one
        let narrow = ctx.content_rect().width() < 700.0;
        if self.was_narrow != Some(narrow) {
            if self.was_narrow.is_some() || narrow {
                self.phone = narrow;
                self.sidebar_open = !narrow;
            }
            self.was_narrow = Some(narrow);
        }
        self.handle_screenshot(&ctx);
        // files dropped on the window: open documents, import models
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for p in dropped {
            if p.extension().map(|e| e.eq_ignore_ascii_case("json")).unwrap_or(false) {
                self.open_path(p);
            } else if matches!(self.tab, crate::workspace::Tab::Studio(_)) {
                self.import_path(&p);
            }
        }
        if self.awaiting_sketch_plane && self.sketch.is_none() && self.picks.iter().any(|r| r.is_planar() || matches!(r, Ref::FeaturePlane { .. })) {
            self.awaiting_sketch_plane = false;
            self.toast = None;
            self.start_sketch();
        }
        self.pump_kernel();
        self.shortcuts(&ctx);

        let header = Frame::new().fill(theme::header()).inner_margin(Margin { left: 12, right: 10, top: 5, bottom: 3 });
        egui::Panel::top("header").frame(header).resizable(false).show_separator_line(false).show(ui, |ui| self.header(ui));
        let bar = Frame::new().fill(theme::panel()).inner_margin(Margin { left: 8, right: 8, top: 2, bottom: 4 });
        egui::Panel::top("toolbar").frame(bar).resizable(false).show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("tabs")
            .frame(Frame::new().fill(theme::tabbar()).inner_margin(Margin { left: 4, right: 8, top: 0, bottom: 0 }))
            .resizable(false)
            .show(ui, |ui| self.tabs_ui(ui));
        if self.phone {
            egui::Panel::bottom("dock").frame(Frame::new().fill(theme::panel()).inner_margin(Margin::same(4))).resizable(false).show(ui, |ui| self.dock(ui));
        }
        if !self.phone && matches!(self.tab, crate::workspace::Tab::Studio(_)) {
            let rail = Frame::new().fill(theme::tabbar());
            egui::Panel::left("rail").frame(rail).resizable(false).exact_size(42.0).show(ui, |ui| self.rail(ui));
        }
        if self.sidebar_open && !self.phone {
            let side = Frame::new().fill(theme::panel());
            egui::Panel::left("sidebar").frame(side).resizable(true).default_size(250.0).min_size(190.0).show(ui, |ui| match self.tab {
                crate::workspace::Tab::Assembly(_) => crate::asm_ui::sidebar(self, ui),
                crate::workspace::Tab::Drawing(_) => crate::drawing_ui::sidebar(self, ui),
                crate::workspace::Tab::Cam(_) => crate::cam_ui::sidebar(self, ui),
                _ => self.sidebar(ui),
            });
        }
        let central = egui::CentralPanel::no_frame().show(ui, |ui| match self.tab {
            crate::workspace::Tab::Assembly(_) => crate::asm_ui::viewport(self, ui),
            crate::workspace::Tab::Drawing(_) => crate::drawing_ui::show(self, ui),
            crate::workspace::Tab::Cam(_) => crate::cam_ui::viewport(self, ui),
            _ => {
                crate::viewport::show(self, ui);
                if self.dialog.is_some() {
                    crate::dialog::show(self, ui);
                }
                crate::script_ui::window(self, ui.ctx());
            }
        });
        if !self.phone && !matches!(self.tab, crate::workspace::Tab::Drawing(_)) {
            self.hint_ui(&ctx, central.response.rect);
        }
        if self.phone && self.sidebar_open {
            self.drawer(&ctx, central.response.rect);
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
    Frame::new().inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 12 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title.to_uppercase()).family(theme::medium()).color(theme::muted()).size(10.5));
        ui.add_space(6.0);
        add(ui);
    });
    let r = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, theme::border()));
}

#[allow(clippy::too_many_arguments)]
/// Eye button on a tree row: shown while the row is hovered, or always when hidden.
/// Returns true when clicked.
fn eye_toggle(ui: &mut Ui, row: &egui::Response, x: f32, hidden: bool, id: impl std::hash::Hash + std::fmt::Debug) -> bool {
    let rect = egui::Rect::from_center_size(egui::pos2(x, row.rect.center().y), egui::vec2(18.0, 18.0));
    let r = ui.interact(rect, ui.id().with(id), Sense::click());
    if !(hidden || row.hovered() || r.hovered()) {
        return false;
    }
    let col = if r.hovered() { theme::accent() } else { theme::muted() };
    icons::image(if hidden { "eyeOff" } else { "eye" }, 14.0).tint(col).paint_at(ui, rect.shrink(2.0));
    r.on_hover_text(if hidden { "Show" } else { "Hide" }).clicked()
}

fn tree_row(ui: &mut Ui, indent: u8, icon: &str, name: &str, picked: bool, rolled: bool, suppressed: bool, hidden: bool, error: Option<&str>) -> egui::Response {
    let h = 26.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, CornerRadius::same(5), theme::sel_fill());
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(5), theme::panel_2());
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
        theme::lerp(theme::text(), theme::muted(), 0.45)
    };
    icons::image(icon, 16.0).tint(col(icol)).paint_at(ui, icon_rect);
    let mut color = if error.is_some() { theme::error_soft() } else { theme::text() };
    if suppressed || hidden {
        color = color.gamma_multiply(0.6);
    }
    let font = egui::FontId::proportional(12.5);
    let galley = ui.painter().layout_no_wrap(name.to_string(), font, col(color));
    let tp = egui::pos2(x0 + 22.0, rect.center().y - galley.size().y / 2.0);
    if suppressed {
        let y = rect.center().y;
        ui.painter().hline(egui::Rangef::new(tp.x, tp.x + galley.size().x), y, Stroke::new(1.0, col(color)));
    }
    ui.painter().galley(tp, galley, col(color));
    if error.is_some() {
        ui.painter().text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, "!", egui::FontId::proportional(13.0), theme::error_soft());
    }
    resp
}

/// A document tab in the bottom bar: icon and name, the active one raised.
fn tab_button(ui: &mut Ui, icon: &str, name: &str, active: bool) -> egui::Response {
    let font = egui::FontId::proportional(12.5);
    let col = if active { theme::text() } else { theme::muted() };
    let font = if active { egui::FontId::new(12.5, theme::medium()) } else { font };
    let g = ui.painter().layout_no_wrap(name.to_string(), font, col);
    let w = (g.size().x + 46.0).max(120.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 32.0), Sense::click());
    let p = ui.painter();
    let body = rect.shrink2(egui::vec2(1.0, 0.0));
    if active {
        p.rect_filled(body, CornerRadius { nw: 0, ne: 0, sw: 6, se: 6 }, theme::panel());
        p.hline(body.shrink2(egui::vec2(6.0, 0.0)).x_range(), rect.top() + 1.0, Stroke::new(2.0, theme::accent()));
    } else if resp.hovered() {
        p.rect_filled(body.shrink2(egui::vec2(0.0, 3.0)), CornerRadius::same(6), theme::panel_2());
    }
    let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(15.0, 15.0));
    icons::image(icon, 15.0).tint(if active { theme::accent() } else { col }).paint_at(ui, ir);
    ui.painter().galley(egui::pos2(rect.left() + 32.0, rect.center().y - g.size().y / 2.0), g, col);
    resp
}

/// A collapsible group header row with a chevron ("Default geometry", "Parts (n)").
fn disclosure_row(ui: &mut Ui, label: &str, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(5), theme::panel_2());
    }
    let ir = egui::Rect::from_center_size(egui::pos2(rect.min.x + 12.0, rect.center().y), egui::vec2(14.0, 14.0));
    icons::image(if open { "chevronDown" } else { "chevronRight" }, 14.0).tint(theme::muted()).paint_at(ui, ir);
    ui.painter().text(egui::pos2(rect.min.x + 24.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::new(12.5, theme::medium()), theme::text());
    resp
}

/// Onshape-style rollback bar: a thick grey bar under the last active feature.
fn rollbar(ui: &mut Ui, rolled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 14.0), Sense::click_and_drag());
    let bar = egui::Rect::from_min_max(egui::pos2(rect.left() + 4.0, rect.center().y - 2.0), egui::pos2(rect.right() - 4.0, rect.center().y + 2.0));
    let c = if rolled {
        theme::accent()
    } else if resp.hovered() {
        theme::muted()
    } else {
        theme::rollbar()
    };
    ui.painter().rect_filled(bar, CornerRadius::same(2), c);
    resp.on_hover_text(if rolled { "Rollback bar: drag it to roll back or forward, click to roll to the end" } else { "Rollback bar: drag it up to roll the model back; new features go here" })
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
