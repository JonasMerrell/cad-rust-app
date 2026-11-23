//! The Scan from photos window (File menu): pick a folder of photos (or a capture-app
//! session), run the reconstruction in the background, then clean it up, set its scale (a
//! reference block found beside the part, two ruler marks clicked on a photo, or a number)
//! and add it to the Part Studio as a mesh reference body. The work is in `photogrammetry.rs`
//! and `photo_mesh.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};

use crate::app::{App, ToastKind};
use crate::doc::Op;
use crate::photo_mesh::Cleanup;
use crate::photogrammetry::{self as pg, Method, Progress, Quality, Reconstruction, Scale, Scan, Settings, Tools};
use crate::theme;

enum Msg {
    Progress(Progress),
    Done(Result<Reconstruction, String>),
}

struct Running {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    step: (usize, usize, String),
    frac: Option<f32>,
    lines: Vec<String>,
    started: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScaleBy {
    Reference,
    Ruler,
    Number,
    Unscaled,
}

/// The photo shown for clicking ruler marks.
struct Shown {
    name: String,
    tex: egui::TextureHandle,
    size: [usize; 2],
}

pub struct PhotoUi {
    pub open: bool,
    folder: String,
    method: Method,
    quality: Quality,
    gpu: bool,
    glomap: bool,
    settings: Settings,
    tools: Option<Tools>,
    show_programs: bool,
    running: Option<Running>,
    recon: Option<Reconstruction>,
    error: Option<String>,
    cleanup: Cleanup,
    scale_by: ScaleBy,
    dims: [f64; 3],
    factor: f64,
    mm: f64,
    /// Undistorted photos to click ruler marks on (calibration photos first).
    photos: Vec<String>,
    photo: usize,
    shown: Option<Shown>,
    zoom: f32,
    clicks: Vec<[f64; 2]>,
    finishing: Option<Receiver<Result<Scan, String>>>,
}

impl Default for PhotoUi {
    fn default() -> Self {
        PhotoUi {
            open: false,
            folder: String::new(),
            method: Method::Auto,
            quality: Quality::Normal,
            gpu: true,
            glomap: true,
            settings: Settings::default(),
            tools: None,
            show_programs: false,
            running: None,
            recon: None,
            error: None,
            cleanup: Cleanup::default(),
            scale_by: ScaleBy::Ruler,
            dims: [25.4, 50.8, 76.2],
            factor: 1.0,
            mm: 100.0,
            photos: vec![],
            photo: 0,
            shown: None,
            zoom: 1.0,
            clicks: vec![],
            finishing: None,
        }
    }
}

/// Open the window.
pub fn open(app: &mut App) {
    let p = &mut app.photo;
    p.open = true;
    if p.tools.is_none() {
        p.settings = Settings::load();
        p.tools = Some(pg::find_tools(&p.settings));
    }
}

/// Capture-app sessions in ~/scans, newest first.
fn sessions() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else { return vec![] };
    let mut v: Vec<PathBuf> = std::fs::read_dir(PathBuf::from(home).join("scans")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v.reverse();
    v.truncate(20);
    v
}

pub fn frame(app: &mut App, ctx: &egui::Context) {
    poll(app, ctx);
    if !app.photo.open {
        return;
    }
    let mut open = true;
    let mut add = false;
    egui::Window::new("Scan from photos")
        .open(&mut open)
        .default_size([560.0, 700.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(280.0, 360.0))
        .constrain(true)
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                let p = &mut app.photo;
                if p.recon.is_some() && p.running.is_none() {
                    add = finish_ui(p, ui);
                } else {
                    setup_ui(p, ui);
                }
            });
        });
    if !open {
        if let Some(r) = &app.photo.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
        app.photo.open = false;
    }
    if add {
        start_finish(app);
    }
}

fn setup_ui(p: &mut PhotoUi, ui: &mut Ui) {
    ui.label(
        RichText::new(
            "Take 20–100 overlapping photos all the way round the part, on a table with some texture (newspaper works). A 1-2-3 block or a ruler beside the part sets the scale.",
        )
        .color(theme::muted()),
    );
    ui.add_space(6.0);
    let busy = p.running.is_some();
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            ui.label("Photos");
            ui.add(egui::TextEdit::singleline(&mut p.folder).desired_width(300.0).hint_text("folder of photos"));
            if ui.button("Browse…").clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                p.folder = d.display().to_string();
            }
            let recent = sessions();
            if !recent.is_empty() {
                ui.menu_button("Captures", |ui| {
                    for s in recent {
                        if ui.button(s.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).clicked() {
                            p.folder = s.display().to_string();
                            ui.close();
                        }
                    }
                });
            }
        });
        let dir = PathBuf::from(p.folder.trim());
        if dir.is_dir() {
            let n = pg::photos_in(&dir).len();
            let mut line = format!("{n} photos");
            if let Some(s) = pg::read_session(&dir) {
                if !s.name.is_empty() {
                    line += &format!(" · {}", s.name);
                }
                if let Some(r) = &s.reference {
                    line += &format!(" · reference: {}", if r.label.is_empty() { &r.kind } else { &r.label });
                }
                if s.complete == Some(false) {
                    line += " · still uploading";
                }
            }
            if dir.join("rcad-scan").join("state.json").is_file() {
                line += " · an earlier run carries on";
            }
            ui.label(RichText::new(line).color(theme::muted()));
        }
        ui.add_space(4.0);
        egui::Grid::new("scan-opts").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Surface");
            egui::ComboBox::from_id_salt("scan-method").selected_text(p.method.label()).width(240.0).show_ui(ui, |ui| {
                for m in Method::ALL {
                    ui.selectable_value(&mut p.method, m, m.label());
                }
            });
            ui.end_row();
            ui.label("Quality");
            ui.horizontal(|ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut p.quality, q, q.label());
                }
            });
            ui.end_row();
            ui.label("");
            ui.horizontal(|ui| {
                let cuda = p.tools.as_ref().is_some_and(|t| t.cuda);
                let mut gpu = p.gpu && cuda;
                if ui.add_enabled(cuda, egui::Checkbox::new(&mut gpu, "Use the GPU")).on_disabled_hover_text("This COLMAP was built without CUDA").changed() {
                    p.gpu = gpu;
                }
                let global = p.tools.as_ref().is_some_and(|t| t.global_mapper || t.glomap.is_some());
                let mut g = p.glomap && global;
                if ui
                    .add_enabled(global, egui::Checkbox::new(&mut g, "Fast global camera solve"))
                    .on_hover_text("Place all the cameras at once instead of one photo at a time: often ten times faster")
                    .on_disabled_hover_text("Needs COLMAP 4 or GLOMAP")
                    .changed()
                {
                    p.glomap = g;
                }
            });
            ui.end_row();
        });
        programs_ui(p, ui);
    });
    ui.add_space(8.0);
    if let Some(r) = &mut p.running {
        let (i, n, name) = &r.step;
        ui.label(RichText::new(format!("Step {} of {n}: {name}", i + 1)).strong());
        let overall = (*i as f32 + r.frac.unwrap_or(0.0)) / (*n).max(1) as f32;
        ui.add(egui::ProgressBar::new(overall).show_percentage());
        let secs = r.started.elapsed().as_secs();
        ui.label(RichText::new(format!("{}:{:02} so far", secs / 60, secs % 60)).color(theme::muted()));
        egui::Frame::new().fill(theme::input_fill()).corner_radius(6.0).inner_margin(6.0).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for l in &r.lines {
                ui.label(RichText::new(l).monospace().size(10.5).color(theme::muted()));
            }
        });
        if ui.button("Stop").clicked() {
            r.cancel.store(true, Ordering::Relaxed);
        }
    } else {
        let ready = p.tools.as_ref().map(|t| p.method.resolve(t)).transpose();
        let ok = Path::new(p.folder.trim()).is_dir() && matches!(ready, Ok(Some(_)));
        if let Err(e) = &ready {
            ui.label(RichText::new(e).color(theme::error()));
        }
        if ui.add_enabled(ok, egui::Button::new(RichText::new("Start").strong())).clicked() {
            start(p);
        }
    }
    if let Some(e) = &p.error {
        ui.add_space(4.0);
        ui.label(RichText::new(e).color(theme::error()));
    }
}

fn programs_ui(p: &mut PhotoUi, ui: &mut Ui) {
    let Some(t) = p.tools.clone() else { return };
    let summary = match (&t.colmap, &t.openmvs) {
        (None, _) => "COLMAP not found".to_string(),
        (Some(_), m) => format!("COLMAP {}{}", if t.cuda { "with CUDA" } else { "(CPU)" }, if m.is_some() { " · OpenMVS" } else { "" }),
    };
    egui::CollapsingHeader::new(format!("Programs: {summary}")).id_salt("scan-programs").default_open(t.colmap.is_none()).open(p.show_programs.then_some(true)).show(ui, |ui| {
        p.show_programs = false;
        let mut changed = false;
        let row = |ui: &mut Ui, label: &str, found: Option<&PathBuf>, value: &mut String, folder: bool, link: (&str, &str)| -> bool {
            let mut ch = false;
            ui.horizontal(|ui| {
                ui.label(label);
                ch |= ui
                    .add(egui::TextEdit::singleline(value).desired_width(250.0).hint_text(found.map(|f| f.display().to_string()).unwrap_or_else(|| "not found".into())))
                    .lost_focus();
                if ui.small_button("…").clicked() {
                    let d = if folder { rfd::FileDialog::new().pick_folder() } else { rfd::FileDialog::new().pick_file() };
                    if let Some(d) = d {
                        *value = d.display().to_string();
                        ch = true;
                    }
                }
                if found.is_none() {
                    ui.hyperlink_to(link.0, link.1);
                }
            });
            ch
        };
        changed |= row(ui, "COLMAP", t.colmap.as_ref(), &mut p.settings.colmap, false, ("get it", "https://github.com/colmap/colmap/releases"));
        changed |= row(ui, "OpenMVS", t.openmvs.as_ref(), &mut p.settings.openmvs, true, ("get it", "https://github.com/cdcseacave/openMVS/releases"));
        changed |= row(ui, "GLOMAP", t.glomap.as_ref(), &mut p.settings.glomap, false, ("optional", "https://github.com/colmap/glomap"));
        if !t.colmap_version.is_empty() {
            ui.label(RichText::new(&t.colmap_version).small().color(theme::muted()));
        }
        ui.label(
            RichText::new("They run as separate programs (COLMAP: BSD licence, OpenMVS: AGPL). Without an NVIDIA GPU, OpenMVS makes the surface on the CPU.")
                .small()
                .color(theme::muted()),
        );
        if changed {
            p.settings.save();
            p.tools = Some(pg::find_tools(&p.settings));
        }
    });
}

fn start(p: &mut PhotoUi) {
    let Some(tools) = p.tools.clone() else { return };
    let job = pg::Job { photos: PathBuf::from(p.folder.trim()), method: p.method, quality: p.quality, gpu: p.gpu, glomap: p.glomap };
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        let (ptx, prx) = channel();
        let tx2 = tx.clone();
        let fwd = std::thread::spawn(move || {
            for m in prx {
                let _ = tx2.send(Msg::Progress(m));
            }
        });
        let r = pg::run(&job, &tools, &ptx, &c);
        drop(ptx);
        let _ = fwd.join();
        let _ = tx.send(Msg::Done(r));
    });
    p.error = None;
    p.running = Some(Running { rx, cancel, step: (0, 1, "Starting".into()), frac: None, lines: vec![], started: Instant::now() });
}

fn poll(app: &mut App, ctx: &egui::Context) {
    let p = &mut app.photo;
    if let Some(r) = &mut p.running {
        ctx.request_repaint_after(Duration::from_millis(250));
        let mut done = None;
        while let Ok(m) = r.rx.try_recv() {
            match m {
                Msg::Progress(Progress::Step(i, n, s)) => {
                    r.step = (i, n, s);
                    r.frac = None;
                }
                Msg::Progress(Progress::Fraction(f)) => r.frac = Some(f),
                Msg::Progress(Progress::Line(l)) => {
                    r.lines.push(l.chars().take(110).collect());
                    if r.lines.len() > 6 {
                        r.lines.remove(0);
                    }
                }
                Msg::Done(d) => done = Some(d),
            }
        }
        if let Some(d) = done {
            p.running = None;
            match d {
                Ok(rec) => {
                    let folder = PathBuf::from(p.folder.trim());
                    let session = pg::read_session(&folder);
                    // calibration photos first, then the rest
                    let cal: Vec<String> = session.as_ref().map(|s| s.calibration_photos.clone()).unwrap_or_default();
                    let mut names: Vec<String> = pg::photos_in(&rec.images);
                    names.sort_by_key(|n| (!cal.contains(n) && !n.starts_with("CAL_"), n.clone()));
                    p.photos = names;
                    p.photo = 0;
                    p.shown = None;
                    p.clicks.clear();
                    let reference = session.as_ref().and_then(|s| s.reference.clone());
                    if let Some(b) = reference.as_ref().and_then(|r| r.block()) {
                        p.dims = b;
                        p.scale_by = ScaleBy::Reference;
                    } else {
                        p.scale_by = ScaleBy::Ruler;
                    }
                    if let Some(mm) = session.as_ref().and_then(|s| s.ruler_mm).or_else(|| reference.filter(|r| r.kind == "ruler").and_then(|r| r.dims_mm.first().copied())) {
                        p.mm = mm;
                    }
                    p.recon = Some(rec);
                    app.flash_info("The reconstruction is done: set the scale and add it", Some(Duration::from_secs(4)));
                }
                Err(e) if e == "stopped" => app.flash_info("Stopped the scan (Start carries on where it was)", Some(Duration::from_secs(4))),
                Err(e) => p.error = Some(e),
            }
        }
    }
    let p = &mut app.photo;
    if let Some(rx) = &p.finishing {
        ctx.request_repaint_after(Duration::from_millis(100));
        if let Ok(r) = rx.try_recv() {
            p.finishing = None;
            match r {
                Ok(scan) => add_scan(app, scan),
                Err(e) => app.photo.error = Some(e),
            }
        }
    }
}

/// Returns true when Add was pressed.
fn finish_ui(p: &mut PhotoUi, ui: &mut Ui) -> bool {
    let Some(rec) = p.recon.clone() else { return false };
    ui.label(RichText::new(format!("Reconstructed with {}. Now clean it up and set its size.", rec.method_label())).strong());
    ui.add_space(4.0);
    ui.checkbox(&mut p.cleanup.remove_table, "Cut away the table and loose bits");
    ui.checkbox(&mut p.cleanup.level, "Stand the part on XY (from the table)");
    ui.horizontal(|ui| {
        ui.label("Keep about");
        ui.add(egui::DragValue::new(&mut p.cleanup.triangles).range(1000..=400000).speed(500.0));
        ui.label("triangles");
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Scale").strong());
    ui.horizontal(|ui| {
        ui.selectable_value(&mut p.scale_by, ScaleBy::Reference, "Reference block");
        ui.selectable_value(&mut p.scale_by, ScaleBy::Ruler, "Ruler marks");
        ui.selectable_value(&mut p.scale_by, ScaleBy::Number, "Number");
        ui.selectable_value(&mut p.scale_by, ScaleBy::Unscaled, "Later");
    });
    match p.scale_by {
        ScaleBy::Reference => {
            ui.label(RichText::new("A block standing on the table beside the part (not touching it) is found and left out. A ball is d × d × d.").color(theme::muted()));
            ui.horizontal(|ui| {
                for k in 0..3 {
                    ui.add(egui::DragValue::new(&mut p.dims[k]).range(0.1..=2000.0).speed(0.1).suffix(" mm"));
                }
                ui.menu_button("Presets", |ui| {
                    for (l, d) in [("1-2-3 block", [25.4, 50.8, 76.2]), ("2-4-6 block", [50.8, 101.6, 152.4]), ("25 mm ball", [25.0; 3])] {
                        if ui.button(l).clicked() {
                            p.dims = d;
                            ui.close();
                        }
                    }
                });
            });
        }
        ScaleBy::Ruler => ruler_ui(p, &rec, ui),
        ScaleBy::Number => {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut p.factor).range(1e-6..=1e6).speed(0.01));
                ui.label("mm per unit");
            });
        }
        ScaleBy::Unscaled => {
            ui.label(
                RichText::new("It comes in at the reconstruction's own size. Measure something you know, then set the Import's scale to known ÷ measured.").color(theme::muted()),
            );
        }
    }
    ui.add_space(8.0);
    let ready = p.scale_by != ScaleBy::Ruler || p.clicks.len() == 2;
    let mut add = false;
    ui.horizontal(|ui| {
        if p.finishing.is_some() {
            ui.spinner();
            ui.label("Cleaning up…");
        } else if ui.add_enabled(ready, egui::Button::new(RichText::new("Add to Part Studio").strong())).on_disabled_hover_text("Click two ruler marks on a photo first").clicked()
        {
            add = true;
        }
        if ui.button("Back").on_hover_text("Change the photos or settings and run again").clicked() {
            p.recon = None;
        }
    });
    if let Some(e) = &p.error {
        ui.label(RichText::new(e).color(theme::error()));
    }
    add
}

fn ruler_ui(p: &mut PhotoUi, rec: &Reconstruction, ui: &mut Ui) {
    ui.label(RichText::new("Click two marks on a ruler (or two corners of anything you know), then enter how far apart they are. Ctrl+scroll zooms.").color(theme::muted()));
    if p.photos.is_empty() {
        ui.label(RichText::new("No undistorted photos were found.").color(theme::error()));
        return;
    }
    ui.horizontal(|ui| {
        if ui.small_button("◀").clicked() {
            p.photo = (p.photo + p.photos.len() - 1) % p.photos.len();
            p.clicks.clear();
        }
        ui.label(format!("{} ({}/{})", p.photos[p.photo], p.photo + 1, p.photos.len()));
        if ui.small_button("▶").clicked() {
            p.photo = (p.photo + 1) % p.photos.len();
            p.clicks.clear();
        }
        ui.separator();
        ui.add(egui::DragValue::new(&mut p.mm).range(0.01..=10000.0).speed(0.5).suffix(" mm"));
        ui.label("apart");
        if !p.clicks.is_empty() && ui.small_button("Clear").clicked() {
            p.clicks.clear();
        }
    });
    let name = p.photos[p.photo].clone();
    if p.shown.as_ref().is_none_or(|s| s.name != name) {
        p.shown = image::open(rec.images.join(&name)).ok().map(|img| {
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let tex = ui.ctx().load_texture(format!("scan-photo:{name}"), egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), egui::TextureOptions::LINEAR);
            Shown { name: name.clone(), tex, size }
        });
    }
    let Some(shown) = &p.shown else {
        ui.label(RichText::new(format!("Can't open {name}")).color(theme::error()));
        return;
    };
    let fit = ui.available_width().max(200.0) / shown.size[0] as f32;
    let scale = fit * p.zoom;
    let disp = egui::vec2(shown.size[0] as f32 * scale, shown.size[1] as f32 * scale);
    let view_h = (ui.available_width() * shown.size[1] as f32 / shown.size[0] as f32).min(320.0);
    egui::ScrollArea::both().id_salt("scan-photo").max_height(view_h).auto_shrink([false, true]).show(ui, |ui| {
        let (rect, resp) = ui.allocate_exact_size(disp, egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.image(shown.tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        if resp.hovered() {
            let dz = ui.input(|i| i.smooth_scroll_delta.y);
            if dz != 0.0 && ui.input(|i| i.modifiers.ctrl || i.modifiers.command) {
                p.zoom = (p.zoom * (1.0 + dz * 0.002)).clamp(1.0, 12.0);
            }
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let px = ((pos - rect.min) / scale).to_pos2();
            if p.clicks.len() >= 2 {
                p.clicks.clear();
            }
            p.clicks.push([px.x as f64, px.y as f64]);
        }
        let to_screen = |c: &[f64; 2]| rect.min + egui::vec2(c[0] as f32, c[1] as f32) * scale;
        let col = Color32::from_rgb(255, 60, 40);
        if p.clicks.len() == 2 {
            painter.line_segment([to_screen(&p.clicks[0]), to_screen(&p.clicks[1])], egui::Stroke::new(1.5, col));
        }
        for (i, c) in p.clicks.iter().enumerate() {
            let s = to_screen(c);
            painter.circle_stroke(s, 6.0, egui::Stroke::new(1.5, col));
            painter.line_segment([s - egui::vec2(10.0, 0.0), s + egui::vec2(10.0, 0.0)], egui::Stroke::new(1.0, col));
            painter.line_segment([s - egui::vec2(0.0, 10.0), s + egui::vec2(0.0, 10.0)], egui::Stroke::new(1.0, col));
            painter.text(s + egui::vec2(8.0, -14.0), egui::Align2::LEFT_BOTTOM, if i == 0 { "A" } else { "B" }, egui::FontId::proportional(13.0), col);
        }
    });
    ui.horizontal(|ui| {
        ui.label("Zoom");
        ui.add(egui::Slider::new(&mut p.zoom, 1.0..=12.0).logarithmic(true).show_value(false));
    });
}

fn start_finish(app: &mut App) {
    let p = &mut app.photo;
    let Some(rec) = p.recon.clone() else { return };
    let scale = match p.scale_by {
        ScaleBy::Reference => Scale::Reference(Some(p.dims)),
        ScaleBy::Ruler => match (p.clicks.first(), p.clicks.get(1)) {
            (Some(a), Some(b)) => Scale::Clicks { photo: p.photos[p.photo].clone(), a: *a, b: *b, mm: p.mm },
            _ => return,
        },
        ScaleBy::Number => Scale::Factor(p.factor),
        ScaleBy::Unscaled => Scale::None,
    };
    let (tx, rx) = channel();
    let (folder, cleanup) = (PathBuf::from(p.folder.trim()), p.cleanup.clone());
    std::thread::spawn(move || {
        let _ = tx.send(pg::finish(&rec, &folder, &cleanup, &scale));
    });
    p.error = None;
    p.finishing = Some(rx);
}

fn add_scan(app: &mut App, scan: Scan) {
    let folder = PathBuf::from(app.photo.folder.trim());
    let base = format!("Scan {}", folder.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default());
    let name = app.doc.unique_name(base.trim());
    app.checkpoint();
    let op = Op::Import {
        name: name.clone(),
        step: String::new(),
        format: "stl".into(),
        data: crate::import::pack(&scan.mesh.to_stl()),
        scale: format!("{}", scan.scale),
        merge: false,
    };
    app.doc.insert(name, op);
    app.switch_tab(crate::workspace::Tab::Studio(app.cur_studio));
    app.first_fit = true;
    app.changed();
    app.photo.open = false;
    let mut note = scan.notes.join("; ");
    if !scan.scaled {
        note += ". Not scaled yet: set the Import's scale";
    }
    app.flash(format!("Added the scan ({} triangles): {note}", scan.mesh.t.len()), if scan.scaled { ToastKind::Info } else { ToastKind::Error });
}

impl Reconstruction {
    fn method_label(&self) -> &'static str {
        self.method.label()
    }
}
