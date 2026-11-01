//! The parts library window of an assembly: standard parts (see `stdparts`) and models
//! downloaded from GrabCAD (see `grabcad`).

use std::time::{Duration, Instant};

use egui::{RichText, Ui};

use crate::app::{App, ToastKind};
use crate::doc::{BodyId, Op};
use crate::stdparts::{self, Standard, STANDARDS};
use crate::theme;

pub struct Lib {
    pub open: bool,
    grabcad: bool,
    search: String,
    std: usize,
    size: f64,
    length: String,
    query: String,
    downloads: Vec<(std::path::PathBuf, std::time::SystemTime)>,
    scanned: Option<Instant>,
    /// Parts waiting for their Part Studio to build before they are inserted.
    pending: Vec<Pending>,
}

impl Default for Lib {
    fn default() -> Self {
        let s = &STANDARDS[0];
        Lib {
            open: false,
            grabcad: false,
            search: String::new(),
            std: 0,
            size: s.default.0,
            length: stdparts::fmt_num(s.default.1),
            query: String::new(),
            downloads: vec![],
            scanned: None,
            pending: vec![],
        }
    }
}

struct Pending {
    studio: usize,
    /// The feature whose parts are inserted.
    feature: crate::doc::FeatId,
    /// One known part, or every part the feature makes.
    body: Option<BodyId>,
    name: String,
    asm: usize,
}

pub fn toggle(app: &mut App) {
    app.asm.lib.open = !app.asm.lib.open;
}

pub fn open_standard(app: &mut App) {
    app.asm.lib.open = true;
    app.asm.lib.grabcad = false;
}

pub fn open_grabcad(app: &mut App) {
    app.asm.lib.open = true;
    app.asm.lib.grabcad = true;
}

fn select(lib: &mut Lib, i: usize) {
    let s = &STANDARDS[i];
    lib.std = i;
    // a size typed in the search ("608", "M8") is picked straight away
    lib.size = lib.search.split_whitespace().find_map(|w| s.parse_size(w)).unwrap_or(s.default.0);
    lib.length = stdparts::fmt_num(s.default.1);
}

/// Put a standard part in the Standard parts Part Studio and insert it into the open assembly
/// once it is built.
pub fn insert_standard(app: &mut App, std: &Standard, size: f64, length: Option<f64>) {
    let Some(asm) = crate::asm_ui::asm_index(app) else { return };
    app.sync_ws();
    let si = stdparts::studio_index(&mut app.ws);
    let live = si == app.cur_studio;
    let doc = if live { &mut app.doc } else { &mut app.ws.studios[si].doc };
    match stdparts::add_to_doc(doc, std, size, length) {
        Ok((body, name, added)) => {
            if added || !app.studio_results.contains_key(&si) {
                rebuild(app, si);
            }
            app.asm.lib.pending.push(Pending { studio: si, feature: body.0, body: Some(body), name, asm });
        }
        Err(e) => app.flash(e, ToastKind::Error),
    }
}

fn rebuild(app: &mut App, si: usize) {
    if si == app.cur_studio {
        app.changed();
    } else {
        app.studio_results.remove(&si);
        let d = app.ws.studios[si].doc.clone();
        let docs = app.studio_docs();
        app.kernel.set_studios(docs);
        app.kernel.rebuild(si, &d);
    }
}

/// Import a downloaded model (STEP, Parasolid, IGES, STL, OBJ or a ZIP of them) into a new Part Studio and
/// insert it into the open assembly: one part as an instance, several as a sub-assembly.
pub fn import_into_assembly(app: &mut App, path: &std::path::Path) {
    let Some(asm) = crate::asm_ui::asm_index(app) else { return };
    let found = match crate::grabcad::read_models(path) {
        Ok(f) => f,
        Err(e) => return app.flash(e, ToastKind::Error),
    };
    if found.files.is_empty() {
        let msg = if found.skipped.is_empty() {
            format!("{} has no STEP, Parasolid, IGES, STL or OBJ files", path.display())
        } else {
            format!(
                "{} only has native CAD files ({}). Download the STEP version instead.",
                path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                found.skipped.join(", ")
            )
        };
        return app.flash(msg, ToastKind::Error);
    }
    app.sync_ws();
    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Import".into());
    let units = app.doc.units;
    let mut doc = crate::doc::Document { units, ..Default::default() };
    let mut feats = vec![];
    for f in &found.files {
        let op = Op::Import { name: f.name.clone(), step: String::new(), format: f.format.into(), data: crate::import::pack(&f.bytes), scale: "1".into(), merge: true };
        feats.push(doc.insert(f.name.clone(), op));
    }
    app.ws.studios.push(crate::workspace::Studio { name: name.clone(), doc });
    let si = app.ws.studios.len() - 1;
    rebuild(app, si);
    for fid in feats {
        let n = if found.files.len() == 1 { name.clone() } else { String::new() };
        app.asm.lib.pending.push(Pending { studio: si, feature: fid, body: None, name: n, asm });
    }
    app.flash_info(format!("Importing {}…", path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()), Some(Duration::from_secs(3)));
}

/// Insert parts whose Part Studio has finished building.
pub fn poll(app: &mut App) {
    if app.asm.lib.pending.is_empty() {
        return;
    }
    let mut keep = vec![];
    let mut ready = vec![];
    for p in std::mem::take(&mut app.asm.lib.pending) {
        let built = if p.studio == app.cur_studio { Some(&app.result) } else { app.studio_results.get(&p.studio) };
        // a stale result does not list the new feature yet
        match built.and_then(|r| r.features.iter().find(|f| f.id == p.feature).map(|f| (r, f.error.clone()))) {
            None => keep.push(p),
            Some((_, Some(e))) => app.flash(format!("{}: {e}", if p.name.is_empty() { "Import" } else { &p.name }), ToastKind::Error),
            Some((r, None)) => {
                let bodies: Vec<BodyId> = r.bodies.iter().map(|b| b.id).filter(|id| p.body.map(|b| b == *id).unwrap_or(id.0 == p.feature)).collect();
                ready.push((p, bodies));
            }
        }
    }
    // the parts of a model of several files go in together: while any of its files is still
    // building, the finished ones wait with it
    let waiting: std::collections::HashSet<usize> = keep.iter().map(|p| p.studio).collect();
    let (ready, later): (Vec<_>, Vec<_>) = ready.into_iter().partition(|(p, _)| p.body.is_some() || !waiting.contains(&p.studio));
    keep.extend(later.into_iter().map(|(p, _)| p));
    app.asm.lib.pending = keep;
    // a model of several parts becomes a sub-assembly so it stays together
    let mut groups: Vec<(usize, usize, Vec<BodyId>)> = vec![];
    for (p, bodies) in ready {
        if crate::asm_ui::asm_index(app) != Some(p.asm) {
            continue;
        }
        if p.body.is_some() || (bodies.len() == 1 && !p.name.is_empty()) {
            for b in bodies {
                let name = part_name(app, p.studio, b, &p.name);
                if p.body.is_none() {
                    name_part(app, p.studio, b, &name);
                }
                crate::asm_ui::insert(app, p.studio, b, None, name);
            }
        } else {
            match groups.iter_mut().find(|g| g.0 == p.studio) {
                Some(g) => g.2.extend(bodies),
                None => groups.push((p.studio, p.asm, bodies)),
            }
        }
    }
    for (si, _, bodies) in groups {
        let name = app.ws.studios[si].name.clone();
        if bodies.is_empty() {
            app.flash(format!("{name}: the import has no solids or surfaces to insert"), ToastKind::Error);
            continue;
        }
        if bodies.len() == 1 {
            let n = part_name(app, si, bodies[0], &name);
            name_part(app, si, bodies[0], &n);
            crate::asm_ui::insert(app, si, bodies[0], None, n);
            continue;
        }
        let mut sub = crate::assembly::Assembly { name: name.clone(), ..Default::default() };
        for (k, b) in bodies.iter().enumerate() {
            let id = sub.new_id();
            // named after its file, numbered when a file holds several parts
            let file = app.ws.studios[si].doc.features.iter().find(|f| f.id == b.0).map(|f| f.name.clone()).unwrap_or_else(|| name.clone());
            let n = if bodies.iter().filter(|o| o.0 == b.0).count() > 1 { format!("{file} {}", b.1 + 1) } else { file };
            let n = part_name(app, si, *b, &n);
            name_part(app, si, *b, &n);
            sub.instances.push(crate::assembly::Instance { id, name: format!("{n} <1>"), studio: si, body: *b, pose: Default::default(), fixed: k == 0, hidden: false, sub: None });
        }
        app.ws.assemblies.push(sub);
        let k = app.ws.assemblies.len() - 1;
        crate::asm_ui::insert(app, 0, (0, 0), Some(k), name);
    }
}

/// Name an imported part in its Part Studio, so the bill of materials shows it.
fn name_part(app: &mut App, studio: usize, b: BodyId, name: &str) {
    if studio != app.cur_studio {
        let p = app.ws.studios[studio].doc.parts.entry(crate::doc::body_key(b)).or_default();
        p.name.get_or_insert_with(|| name.to_string());
    }
}

fn part_name(app: &App, studio: usize, b: BodyId, fallback: &str) -> String {
    app.studio_doc(studio).and_then(|d| d.parts.get(&crate::doc::body_key(b))).and_then(|p| p.name.clone()).unwrap_or_else(|| {
        if fallback.is_empty() {
            app.studio_bodies(studio).iter().find(|m| m.id == b).map(|m| m.name.clone()).unwrap_or_default()
        } else {
            fallback.to_string()
        }
    })
}

pub fn window(app: &mut App, ctx: &egui::Context) {
    poll(app);
    if !app.asm.lib.open {
        return;
    }
    let mut open = true;
    let mut insert: Option<(usize, f64, Option<f64>)> = None;
    let mut import: Option<std::path::PathBuf> = None;
    let mut choose = false;
    egui::Window::new("Parts library")
        .open(&mut open)
        .default_size([520.0, 440.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(260.0, 220.0))
        .constrain(true)
        .collapsible(false)
        .show(ctx, |ui| {
            let lib = &mut app.asm.lib;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut lib.grabcad, false, "Standard parts");
                ui.selectable_value(&mut lib.grabcad, true, "GrabCAD");
            });
            ui.separator();
            if lib.grabcad {
                grabcad_tab(ui, lib, &mut import, &mut choose);
            } else {
                standard_tab(ui, lib, &mut insert);
            }
        });
    if !open {
        app.asm.lib.open = false;
    }
    if let Some((i, size, len)) = insert {
        insert_standard(app, &STANDARDS[i], size, len);
    }
    if choose {
        let exts = ["zip", "step", "stp", "x_t", "igs", "iges", "stl", "obj", "ZIP", "STEP", "STP", "X_T", "IGS", "IGES", "STL", "OBJ"];
        import = app.open_file_dialog("ZIP, STEP, Parasolid, IGES, STL, OBJ", &exts);
    }
    if let Some(p) = import {
        import_into_assembly(app, &p);
    }
}

fn standard_tab(ui: &mut Ui, lib: &mut Lib, insert: &mut Option<(usize, f64, Option<f64>)>) {
    ui.add(egui::TextEdit::singleline(&mut lib.search).hint_text("Search, e.g. cap screw, nut, 608, NEMA").desired_width(f32::INFINITY));
    let q = lib.search.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut pick = None;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(210.0);
            egui::ScrollArea::vertical().id_salt("std-list").max_height(330.0).show(ui, |ui| {
                for cat in stdparts::categories() {
                    let items: Vec<(usize, &Standard)> = STANDARDS
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.category == cat)
                        .filter(|(_, s)| {
                            let sizes: Vec<String> = s.sizes().iter().map(|&v| s.size_label(v).to_lowercase()).collect();
                            let hay = format!("{} {} {} {}", s.key, s.name, s.category, sizes.join(" ")).to_lowercase();
                            words.iter().all(|w| hay.contains(w))
                        })
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(cat).color(theme::muted()).size(11.5).strong());
                    for (i, s) in items {
                        let label = RichText::new(format!("{}  ·  {}", s.name, s.key)).size(12.5);
                        if ui.selectable_label(lib.std == i, label).clicked() {
                            pick = Some(i);
                        }
                    }
                }
            });
        });
        ui.separator();
        ui.vertical(|ui| {
            let s = &STANDARDS[lib.std];
            ui.label(RichText::new(s.name).size(14.0).strong());
            ui.label(RichText::new(format!("{} · {}", s.key, s.category)).color(theme::muted()).size(12.0));
            ui.add_space(6.0);
            egui::Grid::new("std-params").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Size");
                egui::ComboBox::from_id_salt("std-size").selected_text(s.size_label(lib.size)).show_ui(ui, |ui| {
                    for v in s.sizes() {
                        ui.selectable_value(&mut lib.size, v, s.size_label(v));
                    }
                });
                ui.end_row();
                if s.lengths.is_some() {
                    ui.label("Length");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut lib.length).desired_width(60.0));
                        ui.label("mm");
                        egui::ComboBox::from_id_salt("std-len").selected_text("").width(24.0).show_ui(ui, |ui| {
                            for l in s.lengths_for(lib.size) {
                                let t = stdparts::fmt_num(l);
                                if ui.selectable_label(lib.length.trim() == t, &t).clicked() {
                                    lib.length = t;
                                }
                            }
                        });
                    });
                    ui.end_row();
                }
            });
            let len = s.lengths.and_then(|_| lib.length.trim().parse::<f64>().ok());
            ui.add_space(8.0);
            let ok = s.lengths.is_none() || len.map(|l| l > 0.0).unwrap_or(false);
            let title = s.title(lib.size, len);
            if ui
                .add_enabled(ok, egui::Button::new(RichText::new(format!("Insert {title}")).strong()))
                .on_hover_text("Insert an instance into this assembly; click again for more")
                .clicked()
            {
                *insert = Some((lib.std, lib.size, len));
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("Parts go in the Standard parts Part Studio as custom features, so you can change a size there later. Threads are not modelled.")
                    .color(theme::muted())
                    .size(11.5),
            );
        });
    });
    if let Some(i) = pick {
        select(lib, i);
    }
}

fn grabcad_tab(ui: &mut Ui, lib: &mut Lib, import: &mut Option<std::path::PathBuf>, choose: &mut bool) {
    ui.label(RichText::new("GrabCAD has no public API and its downloads need you signed in, so the search opens in your browser. Download the STEP (or the whole ZIP) and insert it from the list below, or drop the file on the window.").size(12.0));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut lib.query).hint_text("e.g. MG996R servo").desired_width(260.0));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Search GrabCAD").on_hover_text("Open the GrabCAD library search (models with STEP files) in your browser").clicked() || enter {
            crate::printing::open_url(&crate::grabcad::search_url(&lib.query));
            lib.scanned = None;
        }
    });
    ui.add_space(8.0);
    // look for new downloads every couple of seconds while the tab is showing
    if lib.scanned.map(|t| t.elapsed() > Duration::from_secs(2)).unwrap_or(true) {
        lib.downloads = crate::grabcad::recent_downloads(Duration::from_secs(48 * 3600));
        lib.scanned = Some(Instant::now());
    }
    ui.ctx().request_repaint_after(Duration::from_secs(2));
    let dir = crate::grabcad::downloads_dir();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Recent downloads").color(theme::muted()).size(11.5).strong());
        if let Some(d) = &dir {
            ui.label(RichText::new(d.display().to_string()).color(theme::muted()).size(11.0));
        }
    });
    egui::ScrollArea::vertical().id_salt("downloads").max_height(220.0).show(ui, |ui| {
        if lib.downloads.is_empty() {
            ui.label(RichText::new("No ZIP, STEP, IGES, STL or OBJ files downloaded in the last two days").color(theme::muted()).italics().size(12.0));
        }
        for (p, t) in &lib.downloads {
            ui.horizontal(|ui| {
                if ui.small_button("Insert").clicked() {
                    *import = Some(p.clone());
                }
                ui.label(RichText::new(p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).size(12.5));
                ui.label(RichText::new(crate::grabcad::age(*t)).color(theme::muted()).size(11.5));
            });
        }
    });
    ui.add_space(6.0);
    if ui.button("Choose a file…").clicked() {
        *choose = true;
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new("GrabCAD models are mostly for personal use: crediting the author is required to share them, and selling them needs the author's permission.")
            .color(theme::muted())
            .size(11.0),
    );
}
