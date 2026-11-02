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
    /// 0 = XY (top), 1 = XZ (front), 2 = YZ (right), 3 = a selected plane or flat face
    pub plane: u8,
    pub flip: bool,
    /// Distance of the cut from the origin along `normal`, in mm.
    pub offset: f64,
    /// The cut plane's normal (unflipped); the side it points to is cut away.
    pub normal: [f64; 3],
}

impl Section {
    pub fn origin_normal(plane: u8) -> [f64; 3] {
        [[0.0, 0.0, 1.0], [0.0, -1.0, 0.0], [1.0, 0.0, 0.0]][plane.min(2) as usize]
    }

    /// A cut through an origin plane, `offset` along its normal.
    pub fn origin(plane: u8, flip: bool, offset: f64) -> Self {
        Section { plane, flip, offset, normal: Self::origin_normal(plane) }
    }

    /// Clip plane for the renderer: keeps points where w - n·p >= 0.
    pub fn clip(&self) -> [f32; 4] {
        let n = glam::DVec3::from(self.normal);
        let (n, w) = if self.flip { (-n, -self.offset) } else { (n, self.offset) };
        [n.x as f32, n.y as f32, n.z as f32, w as f32]
    }

    pub fn keeps(&self, p: glam::DVec3) -> bool {
        let c = self.clip();
        c[3] as f64 - glam::DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64).dot(p) >= 0.0
    }

    pub fn label(&self) -> String {
        let name = ["Top", "Front", "Right", "Selection"][self.plane.min(3) as usize];
        format!("{name}{}", if self.flip { " (flipped)" } else { "" })
    }
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
    pub fea: crate::fea_ui::FeaUi,
    pub pcb: crate::pcb_ui::PcbUi,
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
    pub palette: crate::palette::Palette,
    pub dark: bool,
    pub script_editor: Option<crate::script_ui::ScriptEditor>,
    /// The Claude chat panel.
    pub claude: crate::claude_ui::ClaudeUi,
    pub vcs: crate::vcs_ui::VcsUi,
    /// The start screen with recent documents.
    pub home: crate::home::HomeUi,
    /// Sketch button pressed with no plane selected: the next planar pick starts the sketch.
    pub awaiting_sketch_plane: bool,
    pub was_narrow: Option<bool>,
    /// Model-check problem being pointed at in the viewport.
    pub issue_focus: Option<[f64; 3]>,
    /// Sidebar width (shared by all tabs) and the tab it was last drawn for.
    pub side_w: f32,
    pub side_tab: Option<crate::workspace::Tab>,
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
    /// Unsaved-changes tracking, autosave, file prompts, tab undo and their windows.
    pub session: crate::session::Session,
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
            fea: Default::default(),
            pcb: Default::default(),
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
            palette: Default::default(),
            dark,
            script_editor: None,
            claude: Default::default(),
            vcs: Default::default(),
            home: Default::default(),
            awaiting_sketch_plane: false,
            was_narrow: None,
            issue_focus: None,
            side_w: 300.0,
            side_tab: None,
            print: crate::printing::load(),
            print_view: false,
            print_cache: Default::default(),
            pending_send: None,
            send_result: Default::default(),
            realistic: false,
            decal_images: Default::default(),
            session: Default::default(),
        };
        if let Some(z) = app.session.prefs.zoom.filter(|z| (0.5..=3.0).contains(z)) {
            cc.egui_ctx.set_zoom_factor(z);
        }
        app.mark_saved();
        if let Some(p) = std::env::args().skip(1).find(|a| !a.starts_with('-')) {
            app.open_path(std::path::PathBuf::from(p));
        } else {
            app.home.open = true;
        }
        app.session.recover = crate::session::leftover_autosaves();
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

    // ------------------------------------------------------------ file dialogs

    /// Ask where to save a file. A dialog that comes back at once means the system has none (no
    /// desktop portal), so the file goes next to the document, or in the home folder, instead.
    pub fn save_file_dialog(&mut self, filter: &str, exts: &[&str], name: impl Into<String>) -> Option<std::path::PathBuf> {
        let name = name.into();
        let t = Instant::now();
        let picked = rfd::FileDialog::new().add_filter(filter, exts).set_file_name(name.clone()).save_file();
        if picked.is_some() || t.elapsed() > Duration::from_millis(300) {
            return picked;
        }
        // beside the document (a bare file name's folder is the empty path: the current one)
        let dir = self.path.as_ref().and_then(|p| p.parent().map(|d| if d.as_os_str().is_empty() { std::path::PathBuf::from(".") } else { d.to_path_buf() })).filter(|d| d.is_dir());
        let dir = dir.map(|d| std::fs::canonicalize(&d).unwrap_or(d));
        let dir = dir.or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from)).unwrap_or_else(|| ".".into());
        log::warn!("no file dialog available; saving into {}", dir.display());
        // never overwrite something without asking
        let path = std::path::Path::new(&name);
        let (stem, ext) = (path.file_stem().and_then(|x| x.to_str()).unwrap_or("file"), path.extension().and_then(|x| x.to_str()));
        let mut out = dir.join(&name);
        let mut n = 2;
        while out.exists() {
            out = dir.join(match ext { Some(e) => format!("{stem} ({n}).{e}"), None => format!("{stem} ({n})") });
            n += 1;
        }
        Some(out)
    }

    /// Ask for a file to open; says so when the system has no file dialog.
    pub fn open_file_dialog(&mut self, filter: &str, exts: &[&str]) -> Option<std::path::PathBuf> {
        let t = Instant::now();
        let picked = rfd::FileDialog::new().add_filter(filter, exts).pick_file();
        if picked.is_none() && t.elapsed() < Duration::from_millis(300) {
            self.flash("No file dialog is available on this system (install xdg-desktop-portal)", ToastKind::Error);
        }
        picked
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
        self.session.poke = true;
    }

    pub fn undo(&mut self) {
        // a tab operation (delete, rename, duplicate, restore a version) that was the last change
        self.refresh_hash();
        if self.ws_latest(false) {
            self.ws_undo_step(false);
            return;
        }
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::undo(self);
        }
        if let crate::workspace::Tab::Sim(_) = self.tab {
            return crate::fea_ui::undo(self);
        }
        if let crate::workspace::Tab::Pcb(_) = self.tab {
            return crate::pcb_ui::undo(self);
        }
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            crate::asm_ui::undo(self);
            return self.leave_missing_tab();
        }
        if let Some(s) = &mut self.sketch {
            if let Some(d) = s.undo.pop() {
                s.redo.push(self.doc.clone());
                self.doc = d;
                crate::sketcher::after_undo(self);
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
        self.refresh_hash();
        if self.ws_latest(true) {
            self.ws_undo_step(true);
            return;
        }
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::redo(self);
        }
        if let crate::workspace::Tab::Sim(_) = self.tab {
            return crate::fea_ui::redo(self);
        }
        if let crate::workspace::Tab::Pcb(_) = self.tab {
            return crate::pcb_ui::redo(self);
        }
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            crate::asm_ui::redo(self);
            return self.leave_missing_tab();
        }
        if let Some(s) = &mut self.sketch {
            if let Some(d) = s.redo.pop() {
                s.undo.push(self.doc.clone());
                self.doc = d;
                crate::sketcher::after_undo(self);
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

    /// The shown tab went away (an undo step from before it existed): show the Part Studio.
    fn leave_missing_tab(&mut self) {
        if !self.ws.has_tab(self.tab) {
            self.switch_tab(crate::workspace::Tab::Studio(self.cur_studio));
        }
    }

    /// Drop the undo steps of the Assembly/Drawing, Manufacture and Simulation tabs: they hold
    /// whole tabs and tab numbers, which a tab operation (delete, restore, …) makes wrong.
    pub fn clear_tab_histories(&mut self) {
        self.asm.undo.clear();
        self.asm.redo.clear();
        crate::cam_ui::clear_history(self);
        crate::fea_ui::clear_history(self);
    }

    /// "Undo", or what a tab-level undo would bring back.
    pub fn undo_label(&self) -> String {
        match self.session.ws_undo.last() {
            Some(s) if self.ws_latest(false) => format!("Undo {}", s.label),
            _ => "Undo".into(),
        }
    }

    pub fn can_undo(&self) -> bool {
        if self.ws_latest(false) {
            return true;
        }
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::can_undo(self);
        }
        if let crate::workspace::Tab::Sim(_) = self.tab {
            return crate::fea_ui::can_undo(self);
        }
        if let crate::workspace::Tab::Pcb(_) = self.tab {
            return crate::pcb_ui::can_undo(self);
        }
        if let crate::workspace::Tab::Assembly(_) | crate::workspace::Tab::Drawing(_) = self.tab {
            return !self.asm.undo.is_empty();
        }
        !self.undo.is_empty() || self.sketch.as_ref().map(|s| !s.undo.is_empty()).unwrap_or(false)
    }

    pub fn can_redo(&self) -> bool {
        if self.ws_latest(true) {
            return true;
        }
        if let crate::workspace::Tab::Cam(_) = self.tab {
            return crate::cam_ui::can_redo(self);
        }
        if let crate::workspace::Tab::Sim(_) = self.tab {
            return crate::fea_ui::can_redo(self);
        }
        if let crate::workspace::Tab::Pcb(_) = self.tab {
            return crate::pcb_ui::can_redo(self);
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
                // sketches with loose points (hole centres, pattern points) stay visible for later features
                let loose = if let Op::Sketch { sketch, .. } = &sk.op {
                    let used: std::collections::HashSet<_> = sketch.curves.iter().flat_map(|c| crate::sketch::Sketch::curve_points(&c.geom)).collect();
                    sketch.points.iter().any(|p| !used.contains(&p.id))
                } else {
                    false
                };
                if matches!(sk.op, Op::Sketch { .. }) && !sk.hidden && !loose {
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
        self.camera.set_view(StdView::Iso);
        self.fit_view();
        self.mark_saved();
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
        // (documents from before the PCB ids: each tab gets one, linking it to its 3D studio)
        crate::pcb::ensure_uids(&mut self.ws.pcbs);
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
        self.fea = Default::default();
        self.pcb = Default::default();
        self.first_fit = true;
        self.session.fitted_asm.clear();
        self.tab = crate::workspace::Tab::Studio(0);
        // results still coming for the previous document must not land in this one
        self.kernel.invalidate();
        self.changed();
        // (a damaged or hand-edited file may name a tab that isn't there)
        let active = if self.ws.has_tab(self.ws.active) { self.ws.active } else { crate::workspace::Tab::Studio(0) };
        self.switch_tab(active);
    }

    /// Take the document Claude edited (`studio` is the Part Studio it worked in). The live Part
    /// Studio gets one undo step; new tabs are opened so the user sees them.
    pub fn apply_agent_workspace(&mut self, mut ws: crate::workspace::Workspace, studio: usize) {
        use crate::workspace::Tab;
        if self.sketch.is_some() {
            crate::sketcher::finish(self);
        }
        if self.dialog.is_some() {
            self.dialog_ok();
        }
        if ws.studios.is_empty() {
            return;
        }
        self.sync_ws();
        let counts = |w: &crate::workspace::Workspace| [w.studios.len(), w.assemblies.len(), w.drawings.len(), w.cams.len(), w.sims.len(), w.pcbs.len()];
        let (old, new) = (counts(&self.ws), counts(&ws));
        let cur = self.cur_studio;
        if cur < ws.studios.len() && serde_json::to_string(&ws.studios[cur].doc).ok() != serde_json::to_string(&self.doc).ok() {
            self.checkpoint();
        }
        ws.active = self.tab;
        crate::pcb::ensure_uids(&mut ws.pcbs);
        // the PCB the editor's undo history belongs to, to find it again in Claude's document
        let pcb_before = crate::pcb_ui::history_pcb(self);
        self.ws = ws;
        if cur >= self.ws.studios.len() {
            self.cur_studio = 0;
            self.undo.clear();
            self.redo.clear();
        }
        self.doc = self.ws.studios[self.cur_studio].doc.clone();
        self.studio_results.clear();
        self.kernel.invalidate();
        self.dirty_model = true;
        if old != new {
            self.asm = Default::default();
            self.dwg = Default::default();
            self.cam = Default::default();
            self.fea = Default::default();
        }
        // Claude's change to the PCB is one undo step there; the history stays while its tab does
        crate::pcb_ui::ws_replaced(self, pcb_before, true);
        self.picks.clear();
        if self.result.bodies.is_empty() {
            // frame the first parts Claude makes
            self.first_fit = true;
        }
        self.session.poke = true;
        self.changed();
        // show a tab Claude just made, or the Part Studio it moved to
        let made = (1..6).rev().find(|&k| new[k] > old[k]);
        let t = match made {
            Some(1) => Tab::Assembly(new[1] - 1),
            Some(2) => Tab::Drawing(new[2] - 1),
            Some(3) => Tab::Cam(new[3] - 1),
            Some(4) => Tab::Sim(new[4] - 1),
            Some(5) => Tab::Pcb(new[5] - 1),
            _ if studio != self.cur_studio && studio < new[0] => Tab::Studio(studio),
            _ => match self.tab {
                Tab::Studio(_) => Tab::Studio(self.cur_studio),
                Tab::Assembly(i) if i < new[1] => self.tab,
                Tab::Drawing(i) if i < new[2] => self.tab,
                Tab::Cam(i) if i < new[3] => self.tab,
                Tab::Sim(i) if i < new[4] => self.tab,
                Tab::Pcb(i) if i < new[5] => self.tab,
                _ => Tab::Studio(self.cur_studio),
            },
        };
        // switch_tab rebuilds what other tabs need only when it changes tab
        self.tab = Tab::Studio(self.cur_studio);
        self.switch_tab(t);
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
        if matches!(t, Tab::Assembly(_) | Tab::Drawing(_) | Tab::Cam(_) | Tab::Sim(_)) {
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
            self.fea.scene_dirty = true;
        }
        if let Tab::Pcb(i) = t {
            self.pcb.enter_tab(i);
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

    /// Open a document (asking first about unsaved changes).
    pub fn open_dialog(&mut self) {
        self.guard(crate::session::Pending::Open);
    }

    pub fn open_path(&mut self, p: std::path::PathBuf) {
        if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            // an exported project: unpack it into the projects folder and open its document
            match crate::project::import(&p, &crate::project::default_dir()) {
                Ok(doc) => {
                    self.open_path(doc.clone());
                    if self.path.as_deref() == Some(doc.as_path()) {
                        self.flash_info(format!("Imported the project into {}", doc.parent().map(|d| d.display().to_string()).unwrap_or_default()), Some(Duration::from_secs(4)));
                    }
                }
                Err(e) => self.flash(format!("Could not import {}: {e}", p.display()), ToastKind::Error),
            }
            return;
        }
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| crate::workspace::Workspace::from_json(&s)) {
            Ok(ws) => {
                self.load_workspace(ws);
                crate::home::touch(self, &p);
                self.path = Some(p);
                self.mark_saved();
            }
            Err(e) => self.flash(format!("Could not open {}: {e}", p.display()), ToastKind::Error),
        }
    }

    pub fn save(&mut self, ask: bool) {
        match self.path.clone() {
            Some(p) if !ask => self.save_to(p),
            _ => {
                let name = self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "part.json".into());
                self.with_save_path("Save as", "Part", &["json"], &name, |a, mut p| {
                    // a name typed without the extension would not show up under Open
                    if p.extension().is_none() {
                        p.set_extension("json");
                    }
                    a.save_to(p)
                });
            }
        }
    }

    pub fn save_to(&mut self, p: std::path::PathBuf) {
        self.sync_ws();
        let json = serde_json::to_string_pretty(&self.ws).map_err(|e| e.to_string());
        match json.and_then(|j| crate::session::write_atomic(&p, j.as_bytes()).map_err(|e| e.to_string())) {
            Ok(()) => {
                let committed = crate::vcs_ui::after_save(self, &p);
                let secs = if committed.is_empty() { 2 } else { 4 };
                self.flash_info(format!("Saved {}{committed}", p.display()), Some(Duration::from_secs(secs)));
                self.path = Some(p.clone());
                self.saved_ok(&p);
            }
            Err(e) => {
                // what the save was for (closing, opening another document) doesn't happen
                self.session.after_save = None;
                self.flash(format!("Could not save {}: {e}", p.display()), ToastKind::Error)
            }
        }
    }

    pub fn import_step(&mut self) {
        let exts = ["step", "stp", "x_t", "igs", "iges", "stl", "obj", "dxf", "svg", "zip", "STEP", "STP", "X_T", "IGS", "IGES", "STL", "OBJ", "DXF", "SVG", "ZIP"];
        self.with_open_path("Import", "STEP, Parasolid, IGES, STL, OBJ, DXF, SVG, ZIP", &exts, |a, p| a.import_path(&p));
    }

    pub fn import_path(&mut self, p: &std::path::Path) {
        if crate::sketch_import::is_drawing(p) {
            return self.import_drawing(p);
        }
        if p.extension().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false) {
            // a download (GrabCAD and the like): every model file of its best format
            match crate::grabcad::read_models(p) {
                Ok(f) if !f.files.is_empty() => {
                    self.checkpoint();
                    for m in f.files {
                        let op = Op::Import { name: m.name.clone(), step: String::new(), format: m.format.into(), data: crate::import::pack(&m.bytes), scale: "1".into(), merge: true };
                        self.doc.insert(m.name, op);
                    }
                    self.first_fit = true;
                    self.changed();
                }
                Ok(f) if !f.skipped.is_empty() => self.flash(format!("The ZIP only has native CAD files ({}). Download the STEP version instead.", f.skipped.join(", ")), ToastKind::Error),
                Ok(_) => self.flash("The ZIP has no STEP, Parasolid, IGES, STL or OBJ files", ToastKind::Error),
                Err(e) => self.flash(e, ToastKind::Error),
            }
            return;
        }
        let Some(format) = crate::import::format_of(p) else {
            self.flash("Import STEP, Parasolid (.x_t), IGES, STL, OBJ, DXF, SVG or ZIP files", ToastKind::Info);
            return;
        };
        let name = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("Import".into());
        match std::fs::read(p).map_err(|e| format!("Could not read {}: {e}", p.display())).and_then(|b| crate::import::storable(format, b, &name)) {
            Ok((format, bytes)) => {
                self.checkpoint();
                let op = Op::Import { name: name.clone(), step: String::new(), format: format.into(), data: crate::import::pack(&bytes), scale: "1".into(), merge: true };
                self.doc.insert(name, op);
                self.first_fit = true;
                self.changed();
            }
            Err(e) => self.flash(e, ToastKind::Error),
        }
    }

    pub fn export_obj(&mut self) {
        let name = format!("{}.obj", self.export_stem());
        self.with_save_path("Export OBJ", "OBJ", &["obj"], &name, |a, p| a.export_obj_to(p));
    }

    /// The document's file name without extension, for export file names.
    fn export_stem(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "part".into())
    }

    fn export_obj_to(&mut self, p: std::path::PathBuf) {
        let mut out = String::from("# rCAD\n");
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

    /// The open assembly (with its mates moving) or Part Studio as an interactive page for a web browser.
    pub fn export_web(&mut self) {
        let asm = match self.tab {
            crate::workspace::Tab::Assembly(i) => Some(i),
            _ => None,
        };
        let stem = match asm {
            Some(i) => self.ws.assemblies.get(i).map(|a| a.name.clone()),
            None => self.ws.studios.get(self.cur_studio).map(|s| s.name.clone()),
        }
        .unwrap_or_else(|| "model".into());
        self.with_save_path("Export web viewer", "Web page", &["html"], &format!("{stem}.html"), move |a, p| a.export_web_to(asm, p));
    }

    fn export_web_to(&mut self, asm: Option<usize>, p: std::path::PathBuf) {
        let built = match asm {
            Some(i) => crate::web_asm::assembly_scene(self, i),
            None => {
                let (s, m) = crate::web_asm::studio_scene(self, self.cur_studio);
                Ok((s, m, vec![]))
            }
        };
        match built.and_then(|(s, m, notes)| crate::web_export::write(&s, &m, &p, true).map(|w| (w, notes))) {
            Ok((w, notes)) => {
                let extra = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
                self.flash_info(format!("Exported {} ({:.1} MB){extra}", p.display(), w.bytes as f64 / 1e6), Some(Duration::from_secs(5)))
            }
            Err(e) => self.flash(format!("Could not export: {e}"), ToastKind::Error),
        }
    }

    pub fn export(&mut self, format: &str) {
        let ext = match format {
            "step" => "step",
            "iges" => "igs",
            _ => "stl",
        };
        let format = if format == "stl" { format!("stl@{}", self.print.stl_tolerance) } else { format.to_string() };
        let name = format!("{}.{ext}", self.export_stem());
        self.with_save_path(&format!("Export {}", ext.to_uppercase()), &ext.to_uppercase(), &[ext], &name, move |a, p| {
            a.kernel.export(&a.doc, &format, p.to_string_lossy().to_string());
            a.flash_info(format!("Exporting {}…", p.display()), None);
        });
    }

    /// Make an STL of the parts and hand it to orcaweb.
    pub fn send_to_slicer(&mut self) {
        if self.result.bodies.is_empty() {
            self.flash("There are no parts to print", ToastKind::Info);
            return;
        }
        let name = self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "part".into());
        let tmp = std::env::temp_dir().join(format!("rcad-send-{}.stl", std::process::id()));
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
        let ctx = ctx.clone();
        let name = format!("{}.png", self.export_stem());
        self.with_save_path("Save screenshot", "PNG", &["png"], &name, move |a, p| {
            a.screenshot_pending = Some(p);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        });
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
                    // keep sketch frames in the document in sync (not an undoable edit, nor an unsaved change)
                    let clean = self.content_hash() == self.session.saved_hash;
                    for f in &mut self.doc.features {
                        if let (Op::Sketch { frame, .. }, Some(fr)) = (&mut f.op, r.frames.get(&f.id)) {
                            *frame = *fr;
                        }
                    }
                    // and the persistent names of picked faces and edges (neither)
                    self.doc.apply_ref_updates(&r.refs);
                    // a clean document stays clean
                    if clean && self.content_hash() != self.session.saved_hash {
                        self.mark_saved();
                    }
                    self.result = r;
                    self.print_cache.borrow_mut().clear();
                    self.scene_dirty = true;
                    if self.first_fit && (!self.result.bodies.is_empty() || self.doc.features.iter().any(|f| matches!(f.op, Op::Sketch { .. }) && !f.hidden)) {
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
                // geometry queries are the agent tools'
                Outcome::Queried(_) => {}
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
        // visible sketches only (hidden ones are usually inside a part already)
        let editing = self.sketch.as_ref().map(|s| s.feature);
        for f in self.doc.features.iter().filter(|f| !f.hidden || Some(f.id) == editing) {
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
        if ctrl {
            // each mode handles its own Ctrl combinations first (Ctrl+A in a sketch, …); the
            // handlers ignore other keys while Ctrl is held
            match self.tab {
                crate::workspace::Tab::Assembly(_) => crate::asm_ui::shortcuts(self, ctx),
                crate::workspace::Tab::Drawing(_) => crate::drawing_ui::shortcuts(self, ctx),
                crate::workspace::Tab::Cam(_) => crate::cam_ui::shortcuts(self, ctx),
                crate::workspace::Tab::Sim(_) => crate::fea_ui::shortcuts(self, ctx),
                crate::workspace::Tab::Pcb(_) => crate::pcb_ui::shortcuts(self, ctx),
                _ if self.sketch.is_some() => crate::sketcher::shortcuts(self, ctx),
                _ => {
                    if pressed(Key::A) && self.dialog.is_none() {
                        // select every part
                        self.picks = self.result.bodies.iter().filter(|b| !self.part_hidden(b.id)).map(|b| Ref::Body { body: b.id }).collect();
                        self.scene_dirty = true;
                    }
                }
            }
        }
        if ctrl && pressed(Key::L) {
            crate::claude_ui::toggle(self);
            return;
        }
        if ctrl && pressed(Key::N) {
            self.guard(crate::session::Pending::New);
            return;
        }
        if (!ctrl && pressed(Key::F1)) || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "?"))) {
            self.session.show_shortcuts = !self.session.show_shortcuts;
            return;
        }
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
            crate::workspace::Tab::Sim(_) => return crate::fea_ui::shortcuts(self, ctx),
            crate::workspace::Tab::Pcb(_) => return crate::pcb_ui::shortcuts(self, ctx),
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
            // one mode at a time: measuring ends the printability view
            if self.print_view {
                self.print_view = false;
                self.scene_dirty = true;
            }
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

    /// A run of toolbar buttons on its own rounded chip.
    pub fn group(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
        ui.add_space(5.0);
        Frame::new().fill(theme::panel_2()).corner_radius(10).inner_margin(Margin::symmetric(3, 1)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                add(ui);
            });
        });
        ui.spacing_mut().item_spacing.x = 1.0;
    }

    /// Top strip: logo, document menu and name, theme and phone toggles.
    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if ui.add(egui::Button::image(icons::image("logo", 20.0).tint(theme::accent())).frame(false)).on_hover_text("Home: recent documents").clicked() {
                crate::home::show_home(self);
            }
            if !self.phone {
                ui.label(RichText::new("rCAD").family(theme::medium()).size(15.0).color(theme::text()));
                let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 18.0), Sense::hover());
                ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, theme::border()));
            }
            ui.menu_image_button(icons::image("menu", 18.0).tint(theme::text()), |ui| self.document_menu(ui)).response.on_hover_text("Document menu");
            let name = self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled document".into());
            let name = if self.phone && name.chars().count() > 18 { format!("{}…", name.chars().take(17).collect::<String>()) } else { name };
            let r = ui.label(RichText::new(name).family(theme::medium()).size(14.0).color(theme::text()));
            if self.is_dirty() {
                r.on_hover_text("Unsaved changes (Ctrl+S saves)");
                ui.add_space(-4.0);
                ui.label(RichText::new("•").size(16.0).color(theme::muted())).on_hover_text("Unsaved changes (Ctrl+S saves)");
            }
            if !self.phone {
                // branch badge: the git branch, or a way to start tracking
                crate::vcs_ui::badge(self, ui);
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
                if !self.phone && ui.add(egui::Button::image(icons::image("chat", 17.0).tint(theme::text())).selected(self.claude.open)).on_hover_text("Design with Claude (Ctrl+L)").clicked() {
                    crate::claude_ui::toggle(self);
                }
                if ui.add(egui::Button::image(icons::image("search", 17.0).tint(theme::text()))).on_hover_text("Search commands (S, / or Ctrl+K)").clicked() {
                    crate::palette::open(self);
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
        ui.set_min_width(240.0);
        let mut action = None;
        let item = |ui: &mut Ui, icon: &str, l: &str, keys: &str| {
            let b = egui::Button::image_and_text(icons::image(icon, 15.0).tint(theme::text()), l).shortcut_text(RichText::new(keys).color(theme::muted()));
            ui.add(b).clicked()
        };
        for (a, icon, l, keys) in [("home", "home", "Home (recent documents)", ""), ("new", "file", "New document", "Ctrl+N"), ("project", "folder", "New project…", ""), ("open", "folder", "Open…", "Ctrl+O")] {
            if item(ui, icon, l, keys) {
                action = Some(a);
                ui.close();
            }
        }
        let recent: Vec<std::path::PathBuf> = self.home.recents.iter().take(10).map(|r| r.path.clone()).collect();
        let mut open_recent = None;
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_image_text_button(icons::image("history", 15.0).tint(theme::text()), "Open recent", |ui| {
                for p in &recent {
                    let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    if ui.button(name).on_hover_text(p.display().to_string()).clicked() {
                        open_recent = Some(p.clone());
                        ui.close();
                    }
                }
            });
        });
        for (a, icon, l, keys) in [("json", "save", "Save", "Ctrl+S"), ("saveas", "save", "Save as…", "Ctrl+Shift+S"), ("git", "branch", "Version control (git)…", ""), ("export-project", "export", "Export project (ZIP)…", ""), ("import-project", "download", "Import project (ZIP)…", ""), ("import", "importFile", "Import STEP, IGES, STL, OBJ…", "")] {
            if item(ui, icon, l, keys) {
                action = Some(a);
                ui.close();
            }
        }
        ui.menu_image_text_button(icons::image("export", 15.0).tint(theme::text()), "Export", |ui| {
            for (a, l) in [("step", "STEP"), ("iges", "IGES"), ("stl", "STL"), ("obj", "OBJ"), ("web", "Web viewer (HTML)"), ("png", "Screenshot PNG")] {
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
        ui.add_enabled_ui(self.can_undo(), |ui| {
            if item(ui, "undo", &self.undo_label(), "Ctrl+Z") {
                action = Some("undo");
                ui.close();
            }
        });
        ui.add_enabled_ui(self.can_redo(), |ui| {
            if item(ui, "redo", "Redo", "Ctrl+Shift+Z") {
                action = Some("redo");
                ui.close();
            }
        });
        ui.separator();
        ui.menu_image_text_button(icons::image("settings", 15.0).tint(theme::text()), "Preferences", |ui| {
            ui.set_min_width(220.0);
            let (icon, l) = if self.dark { ("sun", "Light mode") } else { ("moon", "Dark mode") };
            if ui.add(egui::Button::image_and_text(icons::image(icon, 15.0).tint(theme::text()), l)).clicked() {
                self.set_dark(ui.ctx(), !self.dark);
                ui.close();
            }
            ui.horizontal(|ui| {
                ui.label("Interface zoom");
                let mut z = ui.ctx().zoom_factor();
                if ui.add(egui::DragValue::new(&mut z).range(0.5..=3.0).speed(0.01).fixed_decimals(2).suffix("×")).on_hover_text("Also Ctrl + and Ctrl −").changed() {
                    ui.ctx().set_zoom_factor(z);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Units");
                self.units_combo(ui);
            });
        });
        if item(ui, "keyboard", "Keyboard shortcuts", "F1") {
            action = Some("keys");
            ui.close();
        }
        ui.separator();
        if item(ui, "close", "Quit", "") {
            action = Some("quit");
            ui.close();
        }
        if let Some(p) = open_recent {
            self.guard(crate::session::Pending::OpenPath(p));
        }
        match action {
            Some("home") => crate::home::show_home(self),
            Some("new") => self.guard(crate::session::Pending::New),
            Some("open") => self.open_dialog(),
            Some("json") => self.save(false),
            Some("saveas") => self.save(true),
            Some("undo") => self.undo(),
            Some("redo") => self.redo(),
            Some("keys") => self.session.show_shortcuts = true,
            Some("git") => crate::vcs_ui::toggle(self),
            Some("project") => crate::vcs_ui::new_project(self),
            Some("export-project") => crate::vcs_ui::export_project(self),
            Some("import-project") => crate::vcs_ui::import_project(self),
            Some("quit") => self.guard(crate::session::Pending::Close),
            Some("import") => self.import_step(),
            Some(f @ ("step" | "stl" | "iges")) => self.export(f),
            Some("obj") => self.export_obj(),
            Some("web") => self.export_web(),
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
            let undo_tip = format!("{} (Ctrl+Z)", self.undo_label());
            if ui
                .add_enabled(self.can_undo(), egui::Button::image(icons::image("undo", 18.0).tint(theme::text())).min_size(egui::vec2(26.0, 26.0)))
                .on_hover_text(&undo_tip)
                .on_disabled_hover_text("Undo (Ctrl+Z): nothing to undo yet")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.can_redo(), egui::Button::image(icons::image("redo", 18.0).tint(theme::text())).min_size(egui::vec2(26.0, 26.0)))
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .on_disabled_hover_text("Redo (Ctrl+Shift+Z): nothing to redo")
                .clicked()
            {
                self.redo();
            }
            if !self.phone {
                match self.tab {
                    crate::workspace::Tab::Assembly(_) => crate::asm_ui::toolbar(self, ui),
                    crate::workspace::Tab::Drawing(_) => crate::drawing_ui::toolbar(self, ui),
                    crate::workspace::Tab::Cam(_) => crate::cam_ui::toolbar(self, ui),
                    crate::workspace::Tab::Sim(_) => crate::fea_ui::toolbar(self, ui),
                    crate::workspace::Tab::Pcb(_) => crate::pcb_ui::toolbar(self, ui),
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

    /// A section through an origin plane, at the middle of the parts.
    pub fn section_for(&self, plane: u8, flip: bool) -> Section {
        let n = glam::DVec3::from(Section::origin_normal(plane));
        let mut lo = glam::DVec3::splat(f64::INFINITY);
        let mut hi = glam::DVec3::splat(f64::NEG_INFINITY);
        if matches!(self.tab, crate::workspace::Tab::Studio(_)) {
            for b in self.result.bodies.iter().filter(|b| !self.part_hidden(b.id)) {
                lo = lo.min(b.bbox[0].into());
                hi = hi.max(b.bbox[1].into());
            }
        }
        let offset = if lo.x.is_finite() { n.dot((lo + hi) / 2.0) } else { 0.0 };
        Section::origin(plane, flip, offset)
    }

    /// A section through the selected plane or flat face.
    pub fn section_through_selection(&self) -> Option<Section> {
        let fr = self.picks.iter().find_map(|r| self.frame_of(r))?;
        let n = fr.normal();
        Some(Section { plane: 3, flip: false, offset: n.dot(glam::DVec3::from(fr.origin)), normal: n.to_array() })
    }

    /// The section plane picker: origin planes (cut at the middle of the parts) or the selection.
    pub fn section_choices(&mut self, ui: &mut Ui) {
        let mut sec = self.section;
        if ui.selectable_label(sec.is_none(), "No section").clicked() {
            sec = None;
        }
        for (pl, name) in [(0u8, "Top"), (1, "Front"), (2, "Right")] {
            for flip in [false, true] {
                let cur = sec.map(|s| s.plane == pl && s.flip == flip).unwrap_or(false);
                let l = if flip { format!("{name} (flipped)") } else { name.to_string() };
                if ui.selectable_label(cur, l).clicked() {
                    // keep the offset when only flipping
                    sec = Some(match sec {
                        Some(s) if s.plane == pl => Section { flip, ..s },
                        _ => self.section_for(pl, flip),
                    });
                }
            }
        }
        let through = self.section_through_selection();
        let cur = sec.map(|s| s.plane == 3).unwrap_or(false);
        let r = ui.add_enabled(through.is_some() || cur, egui::Button::selectable(cur, "Through the selected plane or face"));
        let r = r.on_disabled_hover_text("Select a plane or flat face first");
        if r.clicked() {
            sec = through.or(sec);
        }
        if sec != self.section {
            self.section = sec;
            self.scene_dirty = true;
            self.asm.scene_dirty = true;
        }
    }

    /// The section's offset (in document units) and flip, shown while a section is on.
    pub fn section_offset_ui(&mut self, ui: &mut Ui) {
        let units = self.doc.units;
        let Some(s) = &mut self.section else { return };
        let mut v = s.offset / units.scale();
        let speed = 0.5 / units.scale();
        let r = ui.add(egui::DragValue::new(&mut v).speed(speed).max_decimals(4).suffix(format!(" {}", units.label())));
        if r.on_hover_text("Section offset along the plane's normal: drag or type").changed() {
            s.offset = v * units.scale();
            self.scene_dirty = true;
            self.asm.scene_dirty = true;
        }
        if ui.add(egui::Button::new("Flip").selected(s.flip)).on_hover_text("Keep the other side").clicked() {
            s.flip = !s.flip;
            self.scene_dirty = true;
            self.asm.scene_dirty = true;
        }
    }

    pub fn section_ui(&mut self, ui: &mut Ui) {
        let label = self.section.map(|s| format!("Section: {}", s.label())).unwrap_or_else(|| "No section".into());
        egui::ComboBox::from_id_salt("section")
            .selected_text(label)
            .width(170.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| self.section_choices(ui))
            .response
            .on_hover_text("Section view");
        self.section_offset_ui(ui);
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
                    // Escape cancels; Enter or clicking elsewhere keeps the new name
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.renaming = None;
                    } else if !r.lost_focus() {
                        if !r.has_focus() {
                            r.request_focus();
                        }
                    } else {
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
            Ref::Face { point, normal, .. } if r.is_planar() => Some(crate::kernel::plane_frame((*point).into(), (*normal).into())),
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
                    // Escape cancels; Enter or clicking elsewhere keeps the new name
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.renaming_part = None;
                    } else if resp.lost_focus() {
                        let t = text.trim().to_string();
                        self.renaming_part = None;
                        self.set_part(id, |p| p.name = (!t.is_empty()).then_some(t));
                    } else if !resp.has_focus() {
                        resp.request_focus();
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
        let Some(p) = self.open_file_dialog("Image", &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"]) else { return };
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
        if all_mass {
            // inertia about the centre of mass: each part's tensor scaled by its density, moved there
            let mut it = glam::DMat3::ZERO;
            for b in &bodies {
                let rho = self.part_props(b.id).material.and_then(|m| doc::MATERIALS.iter().find(|x| x.0 == m).map(|x| x.1)).unwrap_or(0.0) / 1000.0;
                let d = glam::DVec3::from(b.centroid) - c;
                let shift = glam::DMat3::IDENTITY * d.length_squared() - glam::DMat3::from_cols(d * d.x, d * d.y, d * d.z);
                it += glam::DMat3::from_cols_array(&b.inertia) * rho + shift * (b.volume * rho);
            }
            let g = |v: f64| crate::expr::trim(v, 1);
            row(ui, "Inertia (g·mm²)", format!("Ixx {}, Iyy {}, Izz {}", g(it.x_axis.x), g(it.y_axis.y), g(it.z_axis.z)));
            row(ui, "", format!("Ixy {}, Ixz {}, Iyz {}", g(it.y_axis.x), g(it.z_axis.x), g(it.z_axis.y)));
        } else {
            row(ui, "Inertia", "set a material".into());
        }
        row(ui, "Size", format!("{} × {} × {} {}", units.fmt(size.x), units.fmt(size.y), units.fmt(size.z), units.label()));
        if let [b] = bodies[..] {
            let id = b.id;
            self.stock_ui(ui, id);
        }
    }

    /// The raw stock a part is machined from: the suggestion, what it weighs, and how it is worked out.
    fn stock_ui(&mut self, ui: &mut Ui, id: doc::BodyId) {
        use crate::stock_size as ss;
        let props = self.part_props(id);
        let mut pref = props.stock.clone().unwrap_or_default();
        let Some(mesh) = self.result.bodies.iter().find(|b| b.id == id) else { return };
        let volume = mesh.volume;
        let Some(st) = ss::for_part(mesh, &props, self.doc.units) else { return };
        let row = |ui: &mut Ui, k: &str, v: String| {
            ui.horizontal_wrapped(|ui| {
                ui.add_sized([96.0, 16.0], egui::Label::new(RichText::new(k).color(theme::muted()).size(12.0)));
                ui.add(egui::Label::new(RichText::new(v).size(12.0)).wrap());
            });
        };
        let custom = pref.custom.trim().to_string();
        if custom.is_empty() {
            row(ui, "Stock", st.label());
            let density = props.material.as_deref().and_then(|m| doc::MATERIALS.iter().find(|x| x.0 == m)).map(|x| x.1);
            let waste = 100.0 * (1.0 - volume / st.volume().max(1e-9));
            match density {
                Some(d) => row(ui, "", format!("{} g, {}% waste", crate::expr::trim(st.volume() / 1000.0 * d, 1), waste.round())),
                None => row(ui, "", format!("{}% waste", waste.round())),
            }
        } else {
            row(ui, "Stock", custom.clone());
            row(ui, "", format!("suggested {}", st.label()));
        }
        let before = pref.clone();
        let mut start = false;
        egui::CollapsingHeader::new(RichText::new("Stock settings").size(12.0)).id_salt(("stock", id)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 16.0], egui::Label::new(RichText::new("Shape").color(theme::muted()).size(12.0)));
                let cur = ss::SHAPES.iter().find(|s| s.0 == pref.shape).map(|s| s.1).unwrap_or("Least material");
                egui::ComboBox::from_id_salt(("stock-shape", id)).selected_text(cur).show_ui(ui, |ui| {
                    for (k, l) in ss::SHAPES {
                        ui.selectable_value(&mut pref.shape, k.to_string(), l);
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 16.0], egui::Label::new(RichText::new("Size").color(theme::muted()).size(12.0)));
                let cur = ss::STANDARDS.iter().find(|s| s.0 == pref.standard).map(|s| s.1).unwrap_or("Standard (from units)");
                egui::ComboBox::from_id_salt(("stock-std", id)).selected_text(cur).show_ui(ui, |ui| {
                    for (k, l) in ss::STANDARDS {
                        ui.selectable_value(&mut pref.standard, k.to_string(), l);
                    }
                });
            });
            let mut num = |ui: &mut Ui, label: &str, v: &mut f64, tip: &str| {
                ui.horizontal(|ui| {
                    ui.add_sized([96.0, 16.0], egui::Label::new(RichText::new(label).color(theme::muted()).size(12.0)));
                    let r = ui.add(egui::DragValue::new(v).range(0.0..=50.0).speed(0.05).max_decimals(2).suffix(" mm")).on_hover_text(tip);
                    start |= r.drag_started() || r.gained_focus();
                });
            };
            num(ui, "Side allowance", &mut pref.side, "Extra on each side of the width and length (each end of a flat or square bar) and around a round bar, to clean up the sawn edges");
            num(ui, "Face allowance", &mut pref.face, "Extra on each face of the thickness and each end of a round bar, to face it flat; 0 uses the plate as it comes");
            ui.horizontal(|ui| {
                ui.add_sized([96.0, 16.0], egui::Label::new(RichText::new("Own size").color(theme::muted()).size(12.0)));
                let r = ui.add(egui::TextEdit::singleline(&mut pref.custom).hint_text("use the suggestion").desired_width(160.0))
                    .on_hover_text("The stock you will really use, shown in the BOM instead of the suggestion");
                start |= r.gained_focus();
            });
        });
        // undo steps: once when a drag or edit starts, or for a pick from a list
        let changed = pref != before;
        if start || (changed && !ui.ctx().egui_is_using_pointer() && !ui.ctx().egui_wants_keyboard_input()) {
            self.checkpoint();
        }
        if changed {
            let key = doc::body_key(id);
            let mut p = props;
            p.stock = (pref != ss::StockPref::default()).then_some(pref);
            if p == doc::PartProps::default() {
                self.doc.parts.remove(&key);
            } else {
                self.doc.parts.insert(key, p);
            }
            self.scene_dirty = true;
        }
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
            self.solve_sketches();
            self.changed();
        }
    }

    /// Re-solve every sketch after variables or units change, so dimensions that use them move the geometry.
    pub fn solve_sketches(&mut self) {
        let (vars, _) = self.doc.var_values();
        let env = crate::expr::Env { vars: &vars, units: self.doc.units };
        for f in &mut self.doc.features {
            if let Op::Sketch { sketch, .. } = &mut f.op {
                sketch.solve(&env, &[]);
            }
        }
    }

    fn variables(&mut self, ui: &mut Ui) {
        let (vals, errs) = self.doc.var_values();
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
                    // Escape cancels; Enter, Tab or clicking elsewhere keeps the new value
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.editing_var = None;
                    } else if !r.lost_focus() {
                        if !r.has_focus() {
                            r.request_focus();
                        }
                    } else {
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
                        self.solve_sketches();
                        self.changed();
                    }
                } else {
                    // an expression shows what it comes to
                    let shown = match vals.get(&v.name) {
                        Some(x) if v.expr.trim().parse::<f64>().is_err() => format!("{} = {}", v.expr, self.doc.units.fmt(*x)),
                        _ => v.expr.clone(),
                    };
                    let mut txt = RichText::new(shown).monospace();
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
            // a value being edited stays with its variable
            self.editing_var = match self.editing_var.take() {
                Some((k, _)) if k == i => None,
                Some((k, t)) if k > i => Some((k - 1, t)),
                e => e,
            };
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
        } else if let crate::workspace::Tab::Pcb(_) = self.tab {
            crate::pcb_ui::hint(self)
        } else if let crate::workspace::Tab::Sim(_) = self.tab {
            if crate::fea_ui::study(self).is_some_and(|s| s.mode == crate::fea::SimMode::Advanced) {
                "Add boundary conditions and periodic pairs, click their faces, then read the results · hover for values".to_string()
            } else {
                "Add supports and loads, click their faces, then read the results · Space animates, hover for values".to_string()
            }
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
        if msg.is_empty() {
            return;
        }
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
            .on_hover_text("Part Studio units: plain numbers you type and all lengths shown use this unit");
        if u != self.doc.units {
            // existing values keep their size (or the user chooses to rescale)
            self.request_units(u);
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
        // in the order the user arranged them
        let tabs: Vec<(Tab, String, &str)> = self
            .ws
            .tabs()
            .into_iter()
            .map(|t| match t {
                Tab::Studio(i) => (t, self.ws.studios[i].name.clone(), "partStudio"),
                Tab::Assembly(i) => (t, self.ws.assemblies[i].name.clone(), "assembly"),
                Tab::Drawing(i) => (t, self.ws.drawings[i].name.clone(), "drawing"),
                Tab::Cam(i) => (t, self.ws.cams[i].name.clone(), "cam"),
                Tab::Sim(i) => (t, self.ws.sims[i].name.clone(), "simulation"),
                Tab::Pcb(i) => (t, self.ws.pcbs[i].name.clone(), "pcb"),
            })
            .collect();
        let n_tabs = tabs.len();
        let mut switch = None;
        let mut drop_at: Option<(Tab, isize)> = None;
        let drag_id = egui::Id::new("tab-drag");
        let dragging: Option<Tab> = ui.ctx().data(|d| d.get_temp(drag_id));
        let mut tab_rects: Vec<(Tab, egui::Rect)> = vec![];
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
                if ui.add(egui::Button::image_and_text(icons::image("simulation", 15.0).tint(theme::text()), "Simulation")).clicked() {
                    action = Some(("new-sim", Tab::Studio(0)));
                    ui.close();
                }
                if ui.add(egui::Button::image_and_text(icons::image("pcb", 15.0).tint(theme::text()), "PCB")).clicked() {
                    action = Some(("new-pcb", Tab::Studio(0)));
                    ui.close();
                }
            })
            .response
            .on_hover_text("Add a Part Studio, Assembly, Drawing, Manufacture, Simulation or PCB tab");
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
                                // Escape cancels; Enter or clicking elsewhere keeps the new name
                                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    action = Some(("rename-cancel", t));
                                } else if r.lost_focus() {
                                    action = Some(("rename-done", t));
                                } else if !r.has_focus() {
                                    r.request_focus();
                                }
                                continue;
                            }
                        }
                        let active = self.tab == t;
                        let r = tab_button(ui, icon, &name, active);
                        tab_rects.push((t, r.rect));
                        if r.clicked() {
                            switch = Some(t);
                        }
                        if r.double_clicked() {
                            action = Some(("rename", t));
                        }
                        // drag a tab sideways to move it
                        if r.drag_started() {
                            ui.ctx().data_mut(|d| d.insert_temp(drag_id, t));
                        }
                        let pos = tab_rects.len() - 1;
                        r.context_menu(|ui| {
                            if ui.button("Rename").clicked() {
                                action = Some(("rename", t));
                                ui.close();
                            }
                            if ui.button("Duplicate").clicked() {
                                action = Some(("duplicate", t));
                                ui.close();
                            }
                            if pos > 0 && ui.button("Move left").clicked() {
                                action = Some(("move-left", t));
                                ui.close();
                            }
                            if pos + 1 < n_tabs && ui.button("Move right").clicked() {
                                action = Some(("move-right", t));
                                ui.close();
                            }
                            if ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                                action = Some(("delete", t));
                                ui.close();
                            }
                        });
                    }
                    // a dragged tab: show where it goes, and move it there on release
                    if let Some(dt) = dragging {
                        if let Some(p) = ui.ctx().pointer_interact_pos() {
                            let from = tab_rects.iter().position(|(t, _)| *t == dt);
                            let to = tab_rects.iter().position(|(_, r)| p.x < r.center().x).unwrap_or(tab_rects.len());
                            if let (Some(from), Some(x)) = (from, tab_rects.get(to).map(|(_, r)| r.left()).or(tab_rects.last().map(|(_, r)| r.right()))) {
                                let y = tab_rects[0].1.y_range();
                                ui.painter().vline(x, y, Stroke::new(2.0, theme::accent()));
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                if ui.input(|i| i.pointer.any_released()) {
                                    let by = if to > from { to as isize - from as isize - 1 } else { to as isize - from as isize };
                                    drop_at = Some((dt, by));
                                }
                            }
                        }
                        if !ui.input(|i| i.pointer.any_down()) {
                            ui.ctx().data_mut(|d| d.remove::<Tab>(drag_id));
                        }
                    }
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_space(4.0);
                self.units_combo(ui);
                let versions = if self.phone { format!("V{}", self.ws.versions.len()) } else { format!("Versions ({})", self.ws.versions.len()) };
                // a click in the name box must not close it: only a click outside does
                let keep_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                egui::containers::menu::MenuButton::new(RichText::new(versions).size(12.0)).config(keep_open).ui(ui, |ui| {
                    ui.set_min_width(260.0);
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut self.version_name).hint_text("Version name").desired_width(150.0));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.button("Create").clicked() || enter {
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
                            if ui.small_button("Restore").on_hover_text("Go back to this version (Ctrl+Z returns to the current state)").clicked() {
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
        if let Some((t, by)) = drop_at {
            if by != 0 {
                self.ws.move_tab(t, by);
            }
        }
        if let Some(i) = self.restore_version.take() {
            self.sync_ws();
            let mut ws = self.ws.clone();
            match ws.restore(i) {
                Ok(()) => {
                    let name = self.ws.versions.get(i).map(|v| v.name.clone()).unwrap_or_default();
                    self.replace_workspace(ws, format!("restore version {name}"));
                    self.flash_info(format!("Restored version {name} · Ctrl+Z goes back"), Some(Duration::from_secs(3)));
                }
                Err(e) => self.flash(format!("Could not restore: {e}"), ToastKind::Error),
            }
        }
    }

    /// Show `ws` in place of the open workspace (an earlier version of it), as one undoable step:
    /// the current state stays one Ctrl+Z away, and the document counts as changed.
    pub fn replace_workspace(&mut self, ws: crate::workspace::Workspace, label: String) {
        self.sync_ws();
        self.ws_checkpoint(label, None, false);
        // restoring may change the Part Studio list: keep histories only where they still fit
        let mut undo = std::mem::take(&mut self.studio_undo);
        undo.insert(self.cur_studio, (std::mem::take(&mut self.undo), std::mem::take(&mut self.redo)));
        let tab = self.tab;
        let path = self.path.clone();
        let (saved, fitted) = (self.session.saved_hash, std::mem::take(&mut self.session.fitted_asm));
        // histories are kept by Part Studio number: only while the studios are the same ones
        let names = |w: &crate::workspace::Workspace| w.studios.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        if names(&self.ws) != names(&ws) {
            undo.clear();
        }
        self.load_workspace(ws);
        self.first_fit = false;
        self.path = path;
        self.session.saved_hash = saved;
        self.session.fitted_asm = fitted;
        let n = self.ws.studios.len();
        undo.retain(|k, _| *k < n);
        let (u, r) = undo.remove(&self.cur_studio).unwrap_or_default();
        self.undo = u;
        self.redo = r;
        self.studio_undo = undo;
        if tab != self.tab && self.ws.tabs().contains(&tab) {
            self.switch_tab(tab);
        }
        self.ws_mark();
    }

    pub(crate) fn tab_action(&mut self, a: &str, t: crate::workspace::Tab) {
        use crate::workspace::Tab;
        let name_of = |ws: &crate::workspace::Workspace, t: Tab| match t {
            Tab::Studio(i) => ws.studios.get(i).map(|s| s.name.clone()),
            Tab::Assembly(i) => ws.assemblies.get(i).map(|s| s.name.clone()),
            Tab::Drawing(i) => ws.drawings.get(i).map(|s| s.name.clone()),
            Tab::Cam(i) => ws.cams.get(i).map(|s| s.name.clone()),
            Tab::Sim(i) => ws.sims.get(i).map(|s| s.name.clone()),
            Tab::Pcb(i) => ws.pcbs.get(i).map(|s| s.name.clone()),
        };
        match a {
            "rename" => self.renaming_tab = name_of(&self.ws, t).map(|n| (t, n)),
            "rename-cancel" => self.renaming_tab = None,
            "rename-done" => {
                if let Some((_, text)) = self.renaming_tab.take() {
                    let text = text.trim().to_string();
                    if !text.is_empty() && name_of(&self.ws, t).as_deref() != Some(text.as_str()) {
                        self.ws_checkpoint(format!("rename “{}”", name_of(&self.ws, t).unwrap_or_default()), None, false);
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
                                // and the tab that machines its soft jaws
                                for c in self.ws.cams.iter_mut().filter(|c| c.jaws_of == old) {
                                    c.jaws_of = text.clone();
                                }
                            }
                            Tab::Sim(i) => self.ws.sims[i].name = text,
                            // (PCB names stay unique: each has its own "<name> 3D" Part Studio)
                            Tab::Pcb(i) => self.ws.pcbs[i].name = crate::pcb::unique_name(&self.ws.pcbs, &text, Some(i)),
                        }
                        self.ws_mark();
                    }
                }
            }
            "move-left" | "move-right" => {
                self.ws_checkpoint(format!("move “{}”", name_of(&self.ws, t).unwrap_or_default()), None, false);
                self.ws.move_tab(t, if a == "move-left" { -1 } else { 1 });
                self.ws_mark();
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
                // (earlier assembly/drawing undo steps would take the new tab away again)
                self.asm.undo.clear();
                self.asm.redo.clear();
                self.ws.assemblies.push(crate::assembly::Assembly { name: format!("Assembly {n}"), ..Default::default() });
                self.switch_tab(Tab::Assembly(n - 1));
                self.asm.insert_open = true;
            }
            "new-drawing" => {
                let n = self.ws.drawings.len() + 1;
                self.asm.undo.clear();
                self.asm.redo.clear();
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
                // the machine settings of the last setup, else the user's saved machine
                self.ws.cams.push(crate::cam::new_setup(format!("Manufacture {n}"), studio, &self.ws.cams, crate::cam::load_machine()));
                self.cam = Default::default();
                self.switch_tab(Tab::Cam(n - 1));
            }
            "new-sim" => {
                let n = self.ws.sims.len() + 1;
                let studio = match self.tab {
                    Tab::Studio(i) => i,
                    _ => self.cur_studio,
                };
                self.ws.sims.push(crate::fea::Study { name: format!("Simulation {n}"), studio, ..Default::default() });
                self.fea = Default::default();
                self.switch_tab(Tab::Sim(n - 1));
            }
            "new-pcb" => {
                let n = self.ws.pcbs.len() + 1;
                self.ws.pcbs.push(crate::pcb::Pcb::new(crate::pcb::next_name(&self.ws.pcbs)));
                self.pcb = Default::default();
                self.switch_tab(Tab::Pcb(n - 1));
            }
            "duplicate" => {
                self.sync_ws();
                self.ws_checkpoint(format!("duplicate “{}”", name_of(&self.ws, t).unwrap_or_default()), None, matches!(t, Tab::Studio(_)));
                if matches!(t, Tab::Assembly(_) | Tab::Drawing(_)) {
                    self.asm.undo.clear();
                    self.asm.redo.clear();
                }
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
                    Tab::Sim(i) => {
                        let mut s = self.ws.sims[i].clone();
                        s.name = format!("{} copy", s.name);
                        self.ws.sims.push(s);
                    }
                    Tab::Pcb(i) => {
                        let mut s = self.ws.pcbs[i].clone();
                        s.name = crate::pcb::unique_name(&self.ws.pcbs, &format!("{} copy", s.name), None);
                        // a copy is another board, with its own 3D Part Studio
                        s.uid = crate::pcb::new_uid();
                        self.ws.pcbs.push(s);
                    }
                }
                self.ws_mark();
            }
            "delete" => {
                let total = self.ws.studios.len() + self.ws.assemblies.len() + self.ws.drawings.len() + self.ws.cams.len() + self.ws.sims.len() + self.ws.pcbs.len();
                if total <= 1 || (matches!(t, Tab::Studio(_)) && self.ws.studios.len() <= 1) {
                    self.flash("A document needs at least one Part Studio", ToastKind::Info);
                    return;
                }
                // ask first, listing what else it changes
                self.session.confirm_delete = Some(t);
            }
            "delete-confirmed" => {
                self.sync_ws();
                let label = format!("delete “{}”", name_of(&self.ws, t).unwrap_or_default());
                self.ws_checkpoint(label, if let Tab::Studio(i) = t { Some(i) } else { None }, false);
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
                        self.ws.fix_refs_after_studio_removed(i);
                        // rebuilds asked for under the old numbering would land on the wrong studio
                        self.kernel.invalidate();
                        self.dirty_model = true;
                        // Derived features follow their studio; ones from the deleted one now fail
                        for (k, st) in self.ws.studios.iter_mut().enumerate() {
                            let d = if k == self.cur_studio { &mut self.doc } else { &mut st.doc };
                            for f in &mut d.features {
                                if let Op::Derived { studio, .. } = &mut f.op {
                                    *studio = if *studio == i { usize::MAX } else { fix(*studio) };
                                }
                            }
                        }
                        self.tab = Tab::Studio(self.cur_studio);
                    }
                    Tab::Assembly(i) => {
                        // with instances of it in other assemblies, drawing views of it and circuit links
                        self.ws.remove_assembly(i);
                        self.session.fitted_asm.clear();
                    }
                    Tab::Drawing(i) => {
                        self.ws.drawings.remove(i);
                    }
                    Tab::Cam(i) => {
                        self.ws.cams.remove(i);
                        self.cam = Default::default();
                    }
                    Tab::Sim(i) => {
                        self.ws.sims.remove(i);
                        self.fea = Default::default();
                    }
                    Tab::Pcb(i) => {
                        self.ws.pcbs.remove(i);
                        self.pcb = Default::default();
                    }
                }
                // mates to instances that went with the tab
                let pruned: usize = self.ws.assemblies.iter_mut().map(crate::session::prune_orphans).sum();
                if pruned > 0 {
                    log::info!("removed {pruned} mates whose parts were deleted");
                }
                self.ws.forget_tab(t);
                // the other tabs' undo steps hold the old numbering
                self.clear_tab_histories();
                self.asm.scene_dirty = true;
                self.dwg.dirty = true;
                self.ws_mark();
                self.flash_info("Deleted · Ctrl+Z brings it back", Some(Duration::from_secs(3)));
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
                    crate::workspace::Tab::Sim(_) => crate::fea_ui::sidebar(self, ui),
                    crate::workspace::Tab::Pcb(_) => crate::pcb_ui::sidebar(self, ui),
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
                } else if let crate::workspace::Tab::Sim(_) = self.tab {
                    crate::fea_ui::toolbar(self, ui);
                } else if let crate::workspace::Tab::Pcb(_) = self.tab {
                    crate::pcb_ui::toolbar(self, ui);
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
        crate::home::thumbnail(self, &ctx);
        // files dropped on the window: open documents, import models
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        for p in dropped {
            if p.extension().map(|e| e.eq_ignore_ascii_case("json")).unwrap_or(false) {
                self.guard(crate::session::Pending::OpenPath(p));
            } else if matches!(self.tab, crate::workspace::Tab::Studio(_)) {
                self.import_path(&p);
            } else if matches!(self.tab, crate::workspace::Tab::Assembly(_)) {
                crate::stdparts_ui::import_into_assembly(self, &p);
            } else if matches!(self.tab, crate::workspace::Tab::Pcb(_)) && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")) {
                crate::digikey_ui::load_csv(self, &p);
            }
        }
        if self.awaiting_sketch_plane && self.sketch.is_none() && self.picks.iter().any(|r| r.is_planar() || matches!(r, Ref::FeaturePlane { .. })) {
            self.awaiting_sketch_plane = false;
            self.toast = None;
            self.start_sketch();
        }
        self.pump_kernel();
        self.session_frame(&ctx);
        if self.home.open {
            crate::home::ui(self, ui);
            // not the "loading the kernel" note
            if self.toast.as_ref().is_some_and(|t| t.until.is_some()) {
                self.toast_ui(&ctx);
            }
            crate::vcs_ui::frame(self, &ctx);
            crate::session::windows(self, &ctx);
            if ctx.input(|i| i.viewport().title.as_deref() != Some("rCAD")) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("rCAD".into()));
            }
            return;
        }
        // while Claude changes the document the editor is locked (claude_ui), keys included
        if !self.claude.busy() {
            crate::palette::shortcut(self, &ctx);
            self.shortcuts(&ctx);
        }
        // frame an Assembly tab the first time it is shown, once its parts are built
        if let crate::workspace::Tab::Assembly(i) = self.tab {
            let built = (0..self.ws.studios.len()).all(|k| k == self.cur_studio || self.studio_results.contains_key(&k));
            if built && !self.session.fitted_asm.contains(&i) {
                self.session.fitted_asm.insert(i);
                crate::asm_ui::fit(self);
            }
        }

        // panels are cards on a canvas; the header and the tab bar sit on the canvas itself
        ui.painter().rect_filled(ui.max_rect(), 0.0, theme::canvas());
        let header = Frame::new().fill(theme::canvas()).inner_margin(Margin { left: 12, right: 10, top: 5, bottom: 0 });
        egui::Panel::top("header").frame(header).resizable(false).show_separator_line(false).show(ui, |ui| self.header(ui));
        let bar = theme::card(theme::panel(), Margin { left: 8, right: 8, top: 4, bottom: 4 });
        egui::Panel::top("toolbar").frame(bar).resizable(false).show_separator_line(false).show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("tabs")
            .frame(Frame::new().fill(theme::canvas()).inner_margin(Margin { left: 4, right: 8, top: 0, bottom: 4 }))
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| self.tabs_ui(ui));
        if self.phone {
            egui::Panel::bottom("dock").frame(Frame::new().fill(theme::panel()).inner_margin(Margin::same(4))).resizable(false).show(ui, |ui| self.dock(ui));
        }
        if !self.phone && matches!(self.tab, crate::workspace::Tab::Studio(_)) {
            let rail = Frame::new().fill(theme::canvas());
            egui::Panel::left("rail").frame(rail).resizable(false).show_separator_line(false).exact_size(42.0).show(ui, |ui| self.rail(ui));
        }
        if self.sidebar_open && !self.phone {
            let side = theme::card(theme::panel(), Margin::ZERO);
            // one width for every tab: the panel keeps what the user dragged it to
            let switched = self.side_tab != Some(self.tab);
            self.side_tab = Some(self.tab);
            let _ = switched;
            let panel = egui::Panel::left("sidebar").frame(side).resizable(false).show_separator_line(false).exact_size(self.side_w);
            let r = panel.show(ui, |ui| {
                // content may not widen the panel
                ui.set_max_width(ui.available_width());
                match self.tab {
                    crate::workspace::Tab::Assembly(_) => crate::asm_ui::sidebar(self, ui),
                    crate::workspace::Tab::Drawing(_) => crate::drawing_ui::sidebar(self, ui),
                    crate::workspace::Tab::Cam(_) => crate::cam_ui::sidebar(self, ui),
                    crate::workspace::Tab::Sim(_) => crate::fea_ui::sidebar(self, ui),
                    crate::workspace::Tab::Pcb(_) => crate::pcb_ui::sidebar(self, ui),
                    _ => self.sidebar(ui),
                }
            });
            // drag the panel's right edge to resize it
            let edge = egui::Rect::from_min_max(egui::pos2(r.response.rect.right() - 3.0, r.response.rect.top()), egui::pos2(r.response.rect.right() + 3.0, r.response.rect.bottom()));
            let h = ui.interact(edge, egui::Id::new("sidebar-resize"), Sense::drag());
            if h.hovered() || h.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if h.dragged() {
                self.side_w = (self.side_w + h.drag_delta().x).clamp(190.0, 520.0);
            }
        }
        crate::claude_ui::panel(self, ui);
        // the viewport is a card too: inset from the canvas edges, corners rounded below
        let gap = if self.phone { 0 } else { theme::CARD_GAP };
        let side_card = self.sidebar_open && !self.phone;
        let cframe = Frame::new().outer_margin(Margin { left: if side_card { 0 } else { gap }, right: gap, top: 0, bottom: gap });
        let central = egui::CentralPanel::no_frame().frame(cframe).show(ui, |ui| match self.tab {
            crate::workspace::Tab::Assembly(_) => crate::asm_ui::viewport(self, ui),
            crate::workspace::Tab::Drawing(_) => crate::drawing_ui::show(self, ui),
            crate::workspace::Tab::Cam(_) => crate::cam_ui::viewport(self, ui),
            crate::workspace::Tab::Sim(_) => crate::fea_ui::viewport(self, ui),
            crate::workspace::Tab::Pcb(_) => crate::pcb_ui::viewport(self, ui),
            _ => {
                crate::viewport::show(self, ui);
                if self.dialog.is_some() {
                    crate::dialog::show(self, ui);
                }
                crate::script_ui::window(self, ui.ctx());
            }
        });
        if gap > 0 {
            let r = central.response.rect;
            theme::round_corners(&ui.painter_at(r), r, theme::CARD_RADIUS as f32);
        }
        if !self.phone && !matches!(self.tab, crate::workspace::Tab::Drawing(_)) {
            self.hint_ui(&ctx, central.response.rect);
        }
        crate::palette::window(self, &ctx);
        crate::vcs_ui::frame(self, &ctx);
        if self.phone && self.sidebar_open {
            self.drawer(&ctx, central.response.rect);
        }
        self.toast_ui(&ctx);
        crate::session::windows(self, &ctx);
        let name = self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled document".into());
        let title = format!("rCAD · {}{name}", if self.is_dirty() { "• " } else { "" });
        if ctx.input(|i| i.viewport().title.as_deref() != Some(title.as_str())) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
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
    let h = 28.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, CornerRadius::same(8), theme::sel_fill());
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), theme::panel_2());
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
    // dragging moves the tab along the bar
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 32.0), Sense::click_and_drag());
    let p = ui.painter();
    // a pill on the canvas; the active one is a raised card
    let pill = rect.shrink2(egui::vec2(3.0, 4.0));
    if active {
        let mut sh = theme::card_shadow();
        sh.blur = 6;
        p.add(sh.as_shape(pill, CornerRadius::same(12)));
        p.rect_filled(pill, CornerRadius::same(12), theme::panel());
    } else if resp.hovered() {
        p.rect_filled(pill, CornerRadius::same(12), theme::lerp(theme::canvas(), theme::panel(), 0.5));
    }
    let ir = egui::Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), egui::vec2(15.0, 15.0));
    icons::image(icon, 15.0).tint(if active { theme::accent() } else { col }).paint_at(ui, ir);
    ui.painter().galley(egui::pos2(rect.left() + 32.0, rect.center().y - g.size().y / 2.0), g, col);
    resp
}

/// A collapsible group header row with a chevron ("Default geometry", "Parts (n)").
fn disclosure_row(ui: &mut Ui, label: &str, open: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), theme::panel_2());
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
