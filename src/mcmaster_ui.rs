//! The McMaster-Carr tab of an assembly's parts library (see `mcmaster`): search on
//! mcmaster.com in the browser and insert the STEP downloads, or with API access, insert a part
//! by its part number.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};

use crate::app::{App, ToastKind};
use crate::mcmaster::{self, Product, Settings};
use crate::stdparts_ui::Lib;
use crate::theme;

#[derive(Default)]
pub struct State {
    pn: String,
    settings: Option<Settings>,
    draft: Settings,
    show_settings: bool,
    busy: Option<(String, Receiver<mcmaster::Reply>)>,
    status: String,
    /// The last part fetched through the API, for its specifications.
    last: Option<Product>,
    all_downloads: bool,
}

impl State {
    fn settings(&mut self) -> &Settings {
        self.settings.get_or_insert_with(mcmaster::load_settings)
    }
}

pub enum Act {
    Import(std::path::PathBuf),
    Choose,
    Fetch(String),
    ChooseCert,
    Save,
}

pub fn tab(ui: &mut Ui, lib: &mut Lib) -> Option<Act> {
    let mut act = None;
    ui.label(
        RichText::new(
            "McMaster-Carr's terms don't allow other programs to read its website, so the search opens mcmaster.com in your browser. On a part's page choose CAD, 3-D STEP, Download, then insert it from the list below (or drop the file on the window).",
        )
        .size(12.0),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut lib.query).hint_text("e.g. shaft collars, or 91292A113").desired_width(260.0));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Search McMaster-Carr").on_hover_text("Open the McMaster-Carr catalogue (or a part number's page) in your browser").clicked() || enter {
            crate::printing::open_url(&mcmaster::search_url(&lib.query));
            lib.scanned = None;
        }
    });
    ui.add_space(8.0);
    if lib.scanned.map(|t| t.elapsed() > Duration::from_secs(2)).unwrap_or(true) {
        lib.downloads = crate::grabcad::recent_downloads(Duration::from_secs(48 * 3600));
        lib.scanned = Some(Instant::now());
    }
    ui.ctx().request_repaint_after(Duration::from_secs(2));
    let mc = &mut lib.mc;
    ui.horizontal(|ui| {
        ui.label(RichText::new("McMaster-Carr downloads").color(theme::muted()).size(11.5).strong());
        ui.checkbox(&mut mc.all_downloads, RichText::new("Show all downloads").size(11.5));
    });
    let stem = |p: &std::path::Path| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let shown: Vec<_> = lib.downloads.iter().filter(|(p, _)| mc.all_downloads || mcmaster::from_file_stem(&stem(p)).is_some()).collect();
    egui::ScrollArea::vertical().id_salt("mc-downloads").max_height(150.0).show(ui, |ui| {
        if shown.is_empty() {
            ui.label(RichText::new("No McMaster-Carr STEP files downloaded in the last two days").color(theme::muted()).italics().size(12.0));
        }
        for (p, t) in shown {
            ui.horizontal(|ui| {
                if ui.small_button("Insert").clicked() {
                    act = Some(Act::Import(p.clone()));
                }
                match mcmaster::from_file_stem(&stem(p)) {
                    Some((pn, desc)) => {
                        ui.label(RichText::new(&pn).size(12.5).strong());
                        ui.label(RichText::new(desc).size(12.5));
                    }
                    None => {
                        ui.label(RichText::new(p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).size(12.5));
                    }
                }
                ui.label(RichText::new(crate::grabcad::age(*t)).color(theme::muted()).size(11.5));
            });
        }
    });
    ui.add_space(4.0);
    if ui.button("Choose a file…").clicked() {
        act = Some(Act::Choose);
    }
    ui.add_space(6.0);
    ui.separator();
    // ---- the API
    let has = mc.settings().has_creds();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Insert by part number (McMaster-Carr API)").color(theme::muted()).size(11.5).strong());
        if ui.small_button(if mc.show_settings { "Hide settings" } else { "API settings" }).clicked() {
            mc.show_settings = !mc.show_settings;
            if mc.show_settings {
                mc.draft = mc.settings().clone();
            }
        }
    });
    if has {
        ui.horizontal(|ui| {
            let r = ui.add_enabled(mc.busy.is_none(), egui::TextEdit::singleline(&mut mc.pn).hint_text("Part number, e.g. 91292A113").desired_width(200.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let pn = mc.pn.trim().to_ascii_uppercase();
            let ok = mcmaster::is_part_number(&pn) && mc.busy.is_none();
            if (ui.add_enabled(ok, egui::Button::new("Insert")).on_hover_text("Fetch the part's 3-D STEP through the API and insert it").clicked() || (enter && ok)) && mc.busy.is_none() {
                act = Some(Act::Fetch(pn));
            }
            if let Some((pn, _)) = &mc.busy {
                ui.spinner();
                ui.label(RichText::new(format!("Fetching {pn}…")).color(theme::muted()).size(12.0));
            }
        });
    } else if !mc.show_settings {
        ui.label(
            RichText::new(format!(
                "The API is for McMaster-Carr customers it has approved. To apply, email {} from your mcmaster.com account; approved accounts get a client certificate and an API login to enter under API settings.",
                mcmaster::API_EMAIL
            ))
            .size(11.5),
        );
        ui.horizontal(|ui| {
            if ui.link("About the API ↗").clicked() {
                crate::printing::open_url(mcmaster::API_HELP);
            }
            if ui.link(format!("Email {} ↗", mcmaster::API_EMAIL)).clicked() {
                crate::printing::open_url(&format!("mailto:{}?subject=Product%20Information%20API%20access", mcmaster::API_EMAIL));
            }
        });
    }
    if !mc.status.is_empty() {
        ui.label(RichText::new(&mc.status).color(theme::muted()).size(11.5));
    }
    if let Some(p) = &mc.last {
        egui::CollapsingHeader::new(RichText::new(format!("{} {}", p.part_number, p.detail)).size(12.0)).id_salt("mc-last").show(ui, |ui| {
            egui::Grid::new("mc-specs").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
                for (a, v) in &p.specs {
                    ui.label(RichText::new(a).color(theme::muted()).size(11.5));
                    ui.label(RichText::new(v).size(11.5));
                    ui.end_row();
                }
            });
            if ui.link("Open on mcmaster.com ↗").clicked() {
                crate::printing::open_url(&mcmaster::product_url(&p.part_number));
            }
        });
    }
    if mc.show_settings {
        settings_ui(ui, mc, &mut act);
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("McMaster-Carr's CAD is for deciding what to buy from them: their terms don't allow sharing the models, or designs that include them, with others.")
                .color(theme::muted())
                .size(11.0),
        );
        if ui.link(RichText::new("Terms ↗").size(11.0)).clicked() {
            crate::printing::open_url(mcmaster::TERMS);
        }
    });
    act
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([130.0, 18.0], egui::Label::new(RichText::new(label).size(12.0)));
        add(ui);
    });
}

fn settings_ui(ui: &mut Ui, mc: &mut State, act: &mut Option<Act>) {
    let d = &mut mc.draft;
    ui.add_space(4.0);
    row(ui, "API user name", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.username).desired_width(200.0));
    });
    row(ui, "API password", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.password).password(true).desired_width(200.0));
    });
    row(ui, "Client certificate", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.cert).hint_text(".pfx, .p12 or .pem").desired_width(200.0));
        if ui.small_button("Browse…").clicked() {
            *act = Some(Act::ChooseCert);
        }
    });
    row(ui, "Certificate password", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.cert_password).password(true).desired_width(200.0));
    });
    ui.label(
        RichText::new("Kept in your config folder, readable by you only. A .pfx certificate is read with the openssl command; without it, convert it to a .pem (certificate and key) first.")
            .color(theme::muted())
            .size(11.0),
    );
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            *act = Some(Act::Save);
        }
        if ui.button("Cancel").clicked() {
            mc.show_settings = false;
        }
    });
}

/// Do what the tab asked for, and insert parts the API has delivered.
pub fn act(app: &mut App, ctx: &egui::Context, act: Option<Act>) {
    match act {
        None => {}
        Some(Act::Import(p)) => crate::stdparts_ui::import_into_assembly(app, &p),
        Some(Act::Choose) => {
            let exts = ["step", "stp", "zip", "x_t", "STEP", "STP", "ZIP", "X_T"];
            if let Some(p) = app.open_file_dialog("STEP, ZIP, Parasolid", &exts) {
                crate::stdparts_ui::import_into_assembly(app, &p);
            }
        }
        Some(Act::ChooseCert) => {
            let exts = ["pfx", "p12", "pem", "PFX", "P12", "PEM"];
            if let Some(p) = app.open_file_dialog("Client certificate", &exts) {
                app.asm.lib.mc.draft.cert = p.display().to_string();
            }
        }
        Some(Act::Save) => {
            let d = app.asm.lib.mc.draft.clone();
            match mcmaster::save_settings(&d) {
                Ok(_) => {
                    let mc = &mut app.asm.lib.mc;
                    mc.settings = Some(d);
                    mc.show_settings = false;
                    mc.status = if mc.settings().has_creds() { "API settings saved".into() } else { "Saved (incomplete: the API needs a user name, password and certificate)".into() };
                }
                Err(e) => app.flash(format!("Couldn't save the McMaster-Carr settings: {e}"), ToastKind::Error),
            }
        }
        Some(Act::Fetch(pn)) => {
            let set = app.asm.lib.mc.settings().clone();
            let rx = mcmaster::spawn(set, pn.clone(), ctx.clone());
            let mc = &mut app.asm.lib.mc;
            mc.busy = Some((pn, rx));
            mc.status.clear();
        }
    }
}

/// Insert a part the API fetched, once it arrives.
pub fn poll(app: &mut App) {
    let Some((pn, rx)) = &app.asm.lib.mc.busy else { return };
    let pn = pn.clone();
    let reply = match rx.try_recv() {
        Ok(r) => r,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the download stopped".into()),
    };
    app.asm.lib.mc.busy = None;
    let f = match reply {
        Ok(f) => f,
        Err(e) => {
            app.asm.lib.mc.status = format!("{pn}: {e}");
            return app.flash(format!("McMaster-Carr {pn}: {e}"), ToastKind::Error);
        }
    };
    let Some(asm) = crate::asm_ui::asm_index(app) else {
        app.asm.lib.mc.status = format!("{pn} arrived, but no assembly is open to insert it into");
        return;
    };
    let Some(format) = crate::import::format_of(std::path::Path::new(&f.file)) else {
        app.asm.lib.mc.status = format!("{pn}: McMaster sent {}, which rCAD can't import", f.file);
        return;
    };
    let p = f.product;
    let stem = format!("{}_{}", p.part_number, p.short());
    match crate::import::storable(format, f.bytes, &stem) {
        Ok((format, bytes)) => {
            let found = crate::grabcad::Found { files: vec![crate::grabcad::ModelFile { name: stem.clone(), format, bytes }], skipped: vec![] };
            crate::stdparts_ui::import_found(app, asm, found, &stem, &mcmaster::part_name(&p.part_number, &p.short()));
            let mc = &mut app.asm.lib.mc;
            mc.status = format!("Inserted {}", mcmaster::part_name(&p.part_number, &p.detail));
            mc.last = Some(p);
            mc.pn.clear();
        }
        Err(e) => {
            app.asm.lib.mc.status = format!("{pn}: {e}");
            app.flash(format!("McMaster-Carr {pn}: {e}"), ToastKind::Error);
        }
    }
}
