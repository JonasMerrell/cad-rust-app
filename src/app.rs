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
use crate::theme::{self, ACCENT, BORDER, MUTED, PANEL, PANEL_2, TEXT};
use crate::worker::{KernelClient, KernelState, Outcome};

/// Feature buttons in toolbar groups, as in the original (`Am`).
pub const FEATURE_GROUPS: &[&[&str]] = &[
    &["sketch", "plane"],
    &["extrude", "revolve", "sweep", "loft"],
    &["fillet", "chamfer", "shell", "hole", "draft"],
    &["linearPattern", "circularPattern", "mirror", "boolean", "split"],
    &["thicken", "helix", "transform", "deleteFace"],
];
/// Buttons whose label is shown on the desktop toolbar (`jm`).
pub const PRIMARY: &[&str] = &["sketch", "extrude", "revolve", "fillet", "hole"];

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
    pub editing_var: Option<(usize, String)>,
    pub path: Option<std::path::PathBuf>,
    pub viewport_rect: egui::Rect,
    pub nav: Option<crate::viewport::Nav>,
    pub first_fit: bool,
    pub screenshot_pending: Option<std::path::PathBuf>,
    pub context_menu: Option<(egui::Pos2, Option<Ref>)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
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
            editing_var: None,
            path: None,
            viewport_rect: egui::Rect::NOTHING,
            nav: None,
            first_fit: true,
            screenshot_pending: None,
            context_menu: None,
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
        !self.undo.is_empty() || self.sketch.as_ref().map(|s| !s.undo.is_empty()).unwrap_or(false)
    }

    pub fn can_redo(&self) -> bool {
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
        self.checkpoint();
        let units = self.doc.units;
        self.doc = Document { units, ..Default::default() };
        self.path = None;
        self.picks.clear();
        self.dialog = None;
        self.sketch = None;
        self.changed();
    }

    // ------------------------------------------------------------ files

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Part", &["json"]).pick_file() {
            self.open_path(p);
        }
    }

    pub fn open_path(&mut self, p: std::path::PathBuf) {
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Document::from_json(&s)) {
            Ok(d) => {
                self.checkpoint();
                self.doc = d;
                self.path = Some(p);
                self.picks.clear();
                self.dialog = None;
                self.sketch = None;
                self.first_fit = true;
                self.changed();
            }
            Err(e) => self.flash(format!("Could not open {}: {e}", p.display()), ToastKind::Error),
        }
    }

    pub fn save(&mut self, ask: bool) {
        let path = if ask || self.path.is_none() {
            rfd::FileDialog::new().add_filter("Part", &["json"]).set_file_name("part.json").save_file()
        } else {
            self.path.clone()
        };
        if let Some(p) = path {
            match std::fs::write(&p, self.doc.to_json()) {
                Ok(()) => {
                    self.flash_info(format!("Saved {}", p.display()), Some(Duration::from_secs(2)));
                    self.path = Some(p);
                }
                Err(e) => self.flash(format!("Could not save: {e}"), ToastKind::Error),
            }
        }
    }

    pub fn import_step(&mut self) {
        let Some(p) = rfd::FileDialog::new().add_filter("STEP", &["step", "stp", "STEP", "STP"]).pick_file() else { return };
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

    pub fn export(&mut self, format: &str) {
        let ext = if format == "step" { "step" } else { "stl" };
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
                Outcome::Rebuilt(r) => {
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
            self.kernel.rebuild(&self.doc);
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
        let fg = if active { theme::ACTIVE_TEXT } else { TEXT };
        let mut b = if show_label {
            egui::Button::image_and_text(icons::image(icon, 18.0).tint(fg), RichText::new(label).size(12.0).color(fg))
        } else {
            egui::Button::image(icons::image(icon, 18.0).tint(fg))
        };
        if active {
            b = b.fill(Color32::from_rgba_unmultiplied(0x4e, 0xa1, 0xff, 0x2e)).stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x4e, 0xa1, 0xff, 0x80)));
        }
        let r = ui.add_enabled(enabled, b);
        r.on_hover_text(tip)
    }

    fn group(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 24.0), Sense::hover());
        ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, BORDER));
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 1.0;
        add(ui);
        ui.spacing_mut().item_spacing.x = 8.0;
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            if ui.add(egui::Button::new(RichText::new("☰").size(14.0))).on_hover_text("Features and variables").clicked() {
                self.sidebar_open = !self.sidebar_open;
            }
            ui.label(RichText::new("cad-app").strong().size(13.5));
            if !self.phone {
                if self.sketch.is_some() {
                    crate::sketcher::toolbar(self, ui);
                } else {
                    self.model_tools(ui);
                }
            }
            Self::group(ui, |ui| {
                if ui.add_enabled(self.can_undo(), egui::Button::image(icons::image("undo", 18.0).tint(TEXT))).on_hover_text("Undo (Ctrl+Z)").clicked() {
                    self.undo();
                }
                if ui.add_enabled(self.can_redo(), egui::Button::image(icons::image("redo", 18.0).tint(TEXT))).on_hover_text("Redo (Ctrl+Shift+Z)").clicked() {
                    self.redo();
                }
            });
            // right-aligned part
            let mode = if self.phone { ("desktop", "Desktop", "Switch to desktop mode") } else { ("phone", "Phone", "Switch to phone mode") };
            if self.tool_button(ui, mode.0, mode.1, mode.2, true, false, true).clicked() {
                self.phone = !self.phone;
                self.sidebar_open = !self.phone;
            }
            if self.sketch.is_some() {
                Self::group(ui, |ui| {
                    let label = if self.phone { "Finish" } else { "Finish sketch" };
                    let b = egui::Button::image_and_text(icons::image("check", 18.0).tint(Color32::WHITE), RichText::new(label).color(Color32::WHITE).size(12.0)).fill(theme::FINISH);
                    if ui.add(b).on_hover_text("Finish sketch (Enter)").clicked() {
                        crate::sketcher::finish(self);
                    }
                    if ui.add(egui::Button::image(icons::image("close", 18.0).tint(TEXT))).on_hover_text("Discard this sketch session's changes").clicked() {
                        crate::sketcher::cancel(self);
                    }
                });
            } else {
                Self::group(ui, |ui| {
                    self.view_menu(ui);
                    self.file_menu(ui);
                });
            }
        });
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
        Self::group(ui, |ui| {
            if self.tool_button(ui, "dimension", "Dims", "Show dimensions on the model (sketch dimensions and kept measurements)", false, self.show_dims, true).clicked() {
                self.show_dims = !self.show_dims;
                self.scene_dirty = true;
            }
            if self.tool_button(ui, "eye", "X-ray", "See-through bodies (X-ray)", false, self.xray, true).clicked() {
                self.xray = !self.xray;
                self.scene_dirty = true;
            }
            if self.tool_button(ui, "measure", "Measure", "Measure (M)", false, self.measure.is_some(), true).clicked() {
                self.toggle_measure();
            }
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
        });
    }

    fn view_menu(&mut self, ui: &mut Ui) {
        egui::ComboBox::from_id_salt("view")
            .selected_text("View…")
            .width(110.0)
            .show_ui(ui, |ui| {
                for (v, l) in [
                    (StdView::Iso, "Isometric (0)"),
                    (StdView::Front, "Front (1)"),
                    (StdView::Top, "Top (2)"),
                    (StdView::Right, "Right (3)"),
                    (StdView::Back, "Back"),
                    (StdView::Bottom, "Bottom"),
                    (StdView::Left, "Left"),
                ] {
                    if ui.selectable_label(false, l).clicked() {
                        self.camera.set_view(v);
                        self.scene_dirty = true;
                    }
                }
                if ui.selectable_label(false, "Zoom to fit (F)").clicked() {
                    self.fit_view();
                }
            })
            .response
            .on_hover_text("Standard views (1 front, 2 top, 3 right, 0 iso, F fit)");
    }

    fn file_menu(&mut self, ui: &mut Ui) {
        let mut action = None;
        egui::ComboBox::from_id_salt("file")
            .selected_text("File…")
            .width(120.0)
            .show_ui(ui, |ui| {
                for (a, l) in [
                    ("new", "New part"),
                    ("open", "Open .json…"),
                    ("json", "Save .json (Ctrl+S)"),
                    ("import", "Import STEP…"),
                    ("step", "Export STEP"),
                    ("stl", "Export STL"),
                    ("png", "Screenshot PNG"),
                ] {
                    if ui.selectable_label(false, l).clicked() {
                        action = Some(a);
                    }
                }
            })
            .response
            .on_hover_text("File");
        match action {
            Some("new") => self.new_document(),
            Some("open") => self.open_dialog(),
            Some("json") => self.save(false),
            Some("import") => self.import_step(),
            Some(f @ ("step" | "stl")) => self.export(f),
            Some("png") => self.screenshot(ui.ctx()),
            _ => {}
        }
    }

    // ------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            section(ui, "Features", |ui| self.feature_tree(ui));
            section(ui, "Variables", |ui| self.variables(ui));
        });
    }

    fn feature_tree(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 1.0;
        for p in OriginPlane::ALL {
            let r = Ref::OriginPlane { plane: p };
            let picked = self.picks.contains(&r);
            let resp = tree_row(ui, "plane", p.name(), picked, false, false, false, None);
            if resp.clicked() {
                self.pick_from_tree(r.clone(), ui.input(|i| i.modifiers.shift || i.modifiers.command));
            }
            if resp.hovered() && self.hover.as_ref() != Some(&r) {
                self.hover = Some(r);
                self.scene_dirty = true;
            }
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 7.0), Sense::hover());
        dashed_hline(ui, rect.center().y, rect.x_range());

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
            let picked = self.picks.iter().any(|r| matches!(r, Ref::FeaturePlane { feature } if *feature == id));
            let err = self.errors.get(&id).cloned();
            let resp = tree_row(ui, f.op.kind(), &f.name, picked || editing, i >= rb, f.suppressed, f.hidden, err.as_deref());
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
                if ui.button(RichText::new("Delete").color(theme::ERROR_SOFT)).clicked() {
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

        if !self.result.bodies.is_empty() {
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 5.0), Sense::hover());
            dashed_hline(ui, rect.top(), rect.x_range());
            let units = self.doc.units;
            let bodies: Vec<(doc::BodyId, String, f64, f64)> = self.result.bodies.iter().map(|b| (b.id, b.name.clone(), b.volume, b.area)).collect();
            for (id, name, vol, area) in bodies {
                let r = Ref::Body { body: id };
                let picked = self.picks.contains(&r);
                let resp = tree_row(ui, "extrude", &name, picked, false, false, false, None).on_hover_text(format!(
                    "Volume {} · area {}",
                    fmt_volume(vol, units),
                    fmt_area(area, units)
                ));
                if resp.clicked() {
                    self.pick_from_tree(r, ui.input(|i| i.modifiers.shift || i.modifiers.command));
                }
            }
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

    fn variables(&mut self, ui: &mut Ui) {
        let (_, errs) = self.doc.var_values();
        let mut remove = None;
        egui::Grid::new("vars").num_columns(3).spacing([6.0, 4.0]).show(ui, |ui| {
            for (i, v) in self.doc.variables.clone().iter().enumerate() {
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
                        self.doc.variables[i].expr = t;
                        self.changed();
                    }
                } else {
                    let mut txt = RichText::new(&v.expr).monospace();
                    if errs.get(i).map(|e| e.is_some()).unwrap_or(false) {
                        txt = txt.color(theme::ERROR_SOFT);
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
            ui.label(RichText::new("Use variables in any dimension or feature value, e.g. ").color(MUTED).size(12.0));
            ui.label(RichText::new("width / 2").monospace().size(11.5));
            ui.label(RichText::new(".").color(MUTED).size(12.0));
        });
    }

    // ------------------------------------------------------------ status bar

    fn statusbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let msg = if let Some(s) = &self.sketch {
                crate::sketcher::status_text(self, s)
            } else if let Some(m) = &self.measure {
                crate::viewport::measure_text(self, m)
            } else if self.dialog.is_some() {
                "Pick geometry in the viewport for the highlighted field · Enter to accept, Esc to cancel".to_string()
            } else if !self.picks.is_empty() {
                format!("{} selected · start a feature, or Esc to clear", self.picks.len())
            } else {
                "Click faces, edges, planes or sketch regions to select them before starting a feature".to_string()
            };
            ui.label(RichText::new(msg).color(MUTED).size(12.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (txt, col) = match self.kernel.state {
                    KernelState::Loading => ("kernel: loading", MUTED),
                    KernelState::Ready => ("kernel: ready", theme::OK_GREEN),
                    KernelState::Busy => ("kernel: rebuilding", MUTED),
                    KernelState::Error => ("kernel: error", theme::ERROR),
                };
                ui.label(RichText::new(txt).color(col).size(12.0));
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
            });
        });
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
            ToastKind::Info => (theme::TOAST, BORDER),
            ToastKind::Error => (theme::TOAST_ERR, theme::hex(0x7a3a3a)),
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
                    ui.label(RichText::new(text).color(TEXT));
                });
            });
    }

    fn dock(&mut self, ui: &mut Ui) {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.sketch.is_some() {
                    crate::sketcher::toolbar(self, ui);
                } else {
                    self.model_tools(ui);
                    Self::group(ui, |ui| {
                        self.view_menu(ui);
                        self.file_menu(ui);
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

        let bar = Frame::new().fill(PANEL).inner_margin(Margin::symmetric(8, 5)).stroke(Stroke::NONE);
        egui::Panel::top("toolbar").frame(bar).resizable(false).show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(10, 4))).resizable(false).show(ui, |ui| self.statusbar(ui));
        if self.phone {
            egui::Panel::bottom("dock").frame(Frame::new().fill(PANEL).inner_margin(Margin::same(4))).resizable(false).show(ui, |ui| self.dock(ui));
        }
        if self.sidebar_open {
            egui::Panel::left("sidebar")
                .frame(Frame::new().fill(PANEL))
                .resizable(true)
                .default_size(270.0)
                .min_size(200.0)
                .show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            crate::viewport::show(self, ui);
            if let Some(_) = &self.dialog {
                crate::dialog::show(self, ui);
            }
        });
        self.toast_ui(&ctx);
        let title = format!(
            "cad-app · {}",
            self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Sketcher".into())
        );
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
        ui.label(RichText::new(title.to_uppercase()).color(MUTED).size(11.0).strong());
        ui.add_space(4.0);
        add(ui);
    });
    let r = ui.max_rect();
    let y = ui.cursor().top();
    ui.painter().hline(r.x_range(), y, Stroke::new(1.0, BORDER));
}

fn dashed_hline(ui: &Ui, y: f32, x: egui::Rangef) {
    let mut a = x.min;
    while a < x.max {
        ui.painter().hline(egui::Rangef::new(a, (a + 3.0).min(x.max)), y, Stroke::new(1.0, BORDER));
        a += 6.0;
    }
}

#[allow(clippy::too_many_arguments)]
fn tree_row(ui: &mut Ui, icon: &str, name: &str, picked: bool, rolled: bool, suppressed: bool, hidden: bool, error: Option<&str>) -> egui::Response {
    let h = 22.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let p = ui.painter();
    if picked {
        p.rect_filled(rect, CornerRadius::same(5), Color32::from_rgba_unmultiplied(0x4e, 0xa1, 0xff, 0x33));
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(5), PANEL_2);
    }
    let alpha = if rolled { 0.4 } else { 1.0 };
    let col = |c: Color32| c.gamma_multiply(alpha);
    let icon_rect = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 3.0), egui::vec2(16.0, 16.0));
    icons::image(icon, 16.0).tint(col(if hidden { MUTED } else { TEXT })).paint_at(ui, icon_rect);
    let mut color = if error.is_some() { theme::ERROR_SOFT } else { TEXT };
    if suppressed || hidden {
        color = color.gamma_multiply(0.6);
    }
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(name.to_string(), font, col(color));
    let tp = egui::pos2(rect.min.x + 28.0, rect.center().y - galley.size().y / 2.0);
    if suppressed {
        let y = rect.center().y;
        ui.painter().hline(egui::Rangef::new(tp.x, tp.x + galley.size().x), y, Stroke::new(1.0, col(color)));
    }
    ui.painter().galley(tp, galley, col(color));
    if error.is_some() {
        ui.painter().text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, "!", egui::FontId::proportional(13.0), theme::ERROR_SOFT);
    }
    resp
}

fn rollbar(ui: &mut Ui, rolled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
    ui.painter().hline(rect.x_range(), rect.top() + 4.0, Stroke::new(2.0, ACCENT));
    ui.painter().text(egui::pos2(rect.left() + 6.0, rect.top() + 13.0), egui::Align2::LEFT_CENTER, "End of part", egui::FontId::proportional(11.0), MUTED);
    if rolled {
        ui.painter().text(egui::pos2(rect.right() - 6.0, rect.top() + 13.0), egui::Align2::RIGHT_CENTER, "roll to end", egui::FontId::proportional(11.0), ACCENT);
    }
    resp.on_hover_text("Rollback bar: new features are inserted here")
}

fn is_ident(s: &str) -> bool {
    let s = s.trim();
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch.is_alphabetic() || ch == '_') && c.all(|ch| ch.is_alphanumeric() || ch == '_') && !["pi", "e", "mm", "cm", "m", "in", "ft", "deg", "rad"].contains(&s)
}

pub fn fmt_volume(mm3: f64, u: Units) -> String {
    format!("{} {}³", crate::expr::trim(mm3 / u.scale().powi(3), 3), u.label())
}

pub fn fmt_area(mm2: f64, u: Units) -> String {
    format!("{} {}²", crate::expr::trim(mm2 / u.scale().powi(2), 3), u.label())
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
