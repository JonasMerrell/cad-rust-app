//! The PCB tab's Digi-Key section: search (API) or CSV import, results, and using a product for
//! the selected part or placing it as a new one.

use std::sync::mpsc::Receiver;

use egui::{RichText, Ui};

use crate::app::{App, ToastKind};
use crate::cam_ui::row;
use crate::digikey::{self, Job, Product, Reply, Settings};
use crate::pcb_ui::{self, Sel, Tool};
use crate::theme;

#[derive(Default)]
pub struct DkUi {
    settings: Option<Settings>,
    /// The settings being edited (saved with Save).
    draft: Settings,
    pub query: String,
    results: Vec<Product>,
    status: String,
    busy: Option<Receiver<Reply>>,
    show_settings: bool,
    /// Index of the result whose parameters are shown.
    open: Option<usize>,
    /// The product to fill in on each part placed with the Place tool.
    pub placing: Option<Product>,
}

impl DkUi {
    pub fn settings(&mut self) -> &Settings {
        self.settings.get_or_insert_with(digikey::load_settings)
    }
}

fn poll(app: &mut App) {
    let Some(rx) = &app.pcb.dk.busy else { return };
    match rx.try_recv() {
        Ok(Ok(v)) => {
            app.pcb.dk.status = match v.len() {
                0 => "Digi-Key found nothing".into(),
                1 => "1 product".into(),
                n => format!("{n} products"),
            };
            app.pcb.dk.results = v;
            app.pcb.dk.open = None;
            app.pcb.dk.busy = None;
        }
        Ok(Err(e)) => {
            app.pcb.dk.status = e;
            app.pcb.dk.busy = None;
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(_) => app.pcb.dk.busy = None,
    }
}

fn import_csv(app: &mut App) {
    let Some(path) = rfd::FileDialog::new().set_title("Import a Digi-Key CSV").add_filter("CSV", &["csv", "txt", "tsv"]).pick_file() else { return };
    load_csv(app, &path);
}

/// Show the products of a Digi-Key CSV (picked, dropped on the PCB tab, or its path typed in the search box).
pub fn load_csv(app: &mut App, path: &std::path::Path) {
    match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| digikey::parse_csv(&String::from_utf8_lossy(&b))) {
        Ok(v) => {
            app.pcb.dk.status = format!("{} products from {}", v.len(), path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
            app.pcb.dk.results = v;
            app.pcb.dk.open = None;
        }
        Err(e) => app.flash(format!("Digi-Key CSV: {e}"), ToastKind::Error),
    }
}

fn search(app: &mut App, ctx: &egui::Context) {
    let q = app.pcb.dk.query.trim().to_string();
    if q.is_empty() {
        return;
    }
    let as_path = std::path::PathBuf::from(q.trim_matches('"'));
    if as_path.extension().is_some_and(|e| e.eq_ignore_ascii_case("csv")) && as_path.is_file() {
        load_csv(app, &as_path);
        return;
    }
    let set = app.pcb.dk.settings().clone();
    if !set.has_creds() {
        crate::printing::open_url(&digikey::web_search_url(&digikey::part_number_of(&q)));
        app.pcb.dk.status =
            "Opened the search on digikey.com. Download its table (the ⤓ button above the results) and import the CSV here, or add API keys in Settings to search from rCAD."
                .into();
        return;
    }
    // a pasted product link or part number gets its details, words get a search
    let job = if q.contains("digikey.") || (!q.contains(' ') && q.chars().any(|c| c.is_ascii_digit()) && q.len() > 5) { Job::Details(q.clone()) } else { Job::Search(q.clone()) };
    // a part number that isn't one exactly still finds something by keyword
    let job = match job {
        Job::Details(pn) if !pn.contains("digikey.") && !pn.ends_with("-ND") => Job::Search(pn),
        j => j,
    };
    app.pcb.dk.status = "Searching Digi-Key…".into();
    app.pcb.dk.busy = Some(digikey::spawn(set, job, ctx.clone()));
}

/// Fill the selected part from a product.
fn use_for(app: &mut App, i: usize, p: &Product) {
    let mut changed = vec![];
    pcb_ui::edit(app, |pcb| {
        if let Some(part) = pcb.parts.get_mut(i) {
            changed = digikey::apply(part, p, true);
        }
    });
    let r = part_ref(app, i);
    if changed.is_empty() {
        app.flash_info(format!("{r} already matches {}", p.mpn), None);
    } else {
        app.flash_info(format!("{r}: set {} from Digi-Key", changed.join(", ")), None);
    }
}

pub fn section_ui(app: &mut App, ui: &mut Ui) {
    poll(app);
    theme::field_style(ui);
    let has_creds = app.pcb.dk.settings().has_creds();
    let ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 64.0).max(80.0);
        let r = ui.add(egui::TextEdit::singleline(&mut app.pcb.dk.query).hint_text("Part number, link or keywords").desired_width(w));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let busy = app.pcb.dk.busy.is_some();
        let tip = if has_creds { "Search Digi-Key" } else { "Open this search on digikey.com (no API keys set up)" };
        if (ui.add_enabled(!busy, egui::Button::new(if has_creds { "Search" } else { "Open ↗" })).on_hover_text(tip).clicked() || enter) && !busy {
            search(app, &ctx);
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Import CSV…").on_hover_text("A table downloaded from a digikey.com search, a cart or a list").clicked() {
            import_csv(app);
        }
        if ui.selectable_label(app.pcb.dk.show_settings, "Settings").on_hover_text("Your Digi-Key API client ID and secret").clicked() {
            app.pcb.dk.show_settings = !app.pcb.dk.show_settings;
            app.pcb.dk.draft = app.pcb.dk.settings().clone();
        }
        if app.pcb.dk.busy.is_some() {
            ui.spinner();
        }
    });
    if !app.pcb.dk.status.is_empty() {
        ui.label(RichText::new(&app.pcb.dk.status).color(theme::muted()).size(11.5));
    }
    if app.pcb.dk.show_settings {
        settings_ui(app, ui);
    }
    let sel_part = match app.pcb.sel {
        Some(Sel::Part(i)) => Some(i),
        _ => None,
    };
    let sel_ref = sel_part.map(|i| part_ref(app, i));
    let mut act: Option<(usize, bool)> = None;
    let mut toggle = None;
    for (k, p) in app.pcb.dk.results.iter().enumerate() {
        ui.add_space(3.0);
        egui::Frame::new().stroke(egui::Stroke::new(1.0, theme::border())).corner_radius(4.0).inner_margin(egui::Margin::same(5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{}  ·  {}", p.mpn, p.manufacturer)).strong().size(12.5));
            let desc = if p.description.is_empty() { &p.detailed } else { &p.description };
            ui.label(RichText::new(desc).size(11.5));
            let mut meta = vec![];
            if !p.digikey.is_empty() {
                meta.push(p.digikey.clone());
            }
            if let Some(q) = p.stock {
                meta.push(format!("{} in stock", q as u64));
            }
            if let Some(pr) = p.price {
                meta.push(format!("{pr:.3} each"));
            }
            let sym = digikey::symbol_for(p);
            meta.push(format!("→ {sym} {} {}", digikey::value_for(p, sym), digikey::footprint_for(p, sym).unwrap_or_else(|| "(footprint: pick one)".into())));
            ui.label(RichText::new(meta.join(" · ")).color(theme::muted()).size(11.0));
            ui.horizontal_wrapped(|ui| {
                if let Some(r) = &sel_ref {
                    if ui
                        .small_button(format!("Use for {r}"))
                        .on_hover_text("Fill the selected part: value, footprint, MPN, manufacturer, description, datasheet, Digi-Key number and BOM fields")
                        .clicked()
                    {
                        act = Some((k, false));
                    }
                }
                if ui.small_button("Place").on_hover_text("Place it on the schematic as a new part").clicked() {
                    act = Some((k, true));
                }
                if !p.datasheet.is_empty() && ui.small_button("Datasheet ↗").clicked() {
                    crate::printing::open_url(&p.datasheet);
                }
                let page = if p.url.is_empty() { digikey::web_search_url(if p.digikey.is_empty() { &p.mpn } else { &p.digikey }) } else { p.url.clone() };
                if ui.small_button("Digi-Key ↗").clicked() {
                    crate::printing::open_url(&page);
                }
                if !p.params.is_empty() && ui.small_button(if app.pcb.dk.open == Some(k) { "Less" } else { "More" }).clicked() {
                    toggle = Some(k);
                }
            });
            if app.pcb.dk.open == Some(k) {
                egui::Grid::new(("dk-params", k)).num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
                    for (n, v) in &p.params {
                        ui.label(RichText::new(n).color(theme::muted()).size(11.0));
                        ui.label(RichText::new(v).size(11.0));
                        ui.end_row();
                    }
                });
            }
        });
    }
    if let Some(k) = toggle {
        app.pcb.dk.open = if app.pcb.dk.open == Some(k) { None } else { Some(k) };
    }
    if let Some((k, place)) = act {
        let p = app.pcb.dk.results[k].clone();
        if place {
            let sym = digikey::symbol_for(&p);
            if app.pcb.view != pcb_ui::View::Schematic {
                pcb_ui::set_view(app, pcb_ui::View::Schematic);
            }
            pcb_ui::start_place(app, sym);
            app.pcb.dk.placing = Some(p);
        } else if let Some(i) = sel_part {
            use_for(app, i, &p);
        }
    }
}

fn settings_ui(app: &mut App, ui: &mut Ui) {
    let d = &mut app.pcb.dk.draft;
    ui.add_space(4.0);
    ui.label(
        RichText::new("Searching from rCAD uses Digi-Key's Product Information API with your own free API app. Without one, search on digikey.com and import its table as CSV.")
            .color(theme::muted())
            .size(11.0),
    );
    if ui
        .link("Create an app at developer.digikey.com ↗")
        .on_hover_text("Sign in, create an Organization and a Production app with the Product Information V4 API, then copy its Client ID and Secret")
        .clicked()
    {
        crate::printing::open_url(digikey::DEV_PORTAL);
    }
    row(ui, "Client ID", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.client_id).desired_width(150.0));
    });
    row(ui, "Client secret", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.client_secret).password(true).desired_width(150.0));
    });
    row(ui, "Site, currency", |ui| {
        ui.add(egui::TextEdit::singleline(&mut d.site).hint_text("US").desired_width(40.0));
        ui.add(egui::TextEdit::singleline(&mut d.currency).hint_text("USD").desired_width(48.0));
    });
    let env = d.client_id.trim().is_empty() && Settings::default().has_creds();
    if env {
        ui.label(RichText::new("Using DIGIKEY_CLIENT_ID and DIGIKEY_CLIENT_SECRET from the environment").color(theme::muted()).size(11.0));
    }
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            let d = app.pcb.dk.draft.clone();
            match digikey::save_settings(&d) {
                Ok(_) => {
                    app.pcb.dk.settings = Some(d);
                    app.pcb.dk.show_settings = false;
                    app.pcb.dk.status = if app.pcb.dk.settings().has_creds() { "Digi-Key keys saved".into() } else { "Saved (no keys: searches open digikey.com)".into() };
                }
                Err(e) => app.flash(format!("Couldn't save the Digi-Key settings: {e}"), ToastKind::Error),
            }
        }
        if ui.button("Cancel").clicked() {
            app.pcb.dk.show_settings = false;
        }
    });
}

/// A part just placed with the Place tool, filled from the product being placed.
pub fn fill_placed(part: &mut crate::pcb::Part, p: &Product) {
    digikey::apply(part, p, true);
    part.pin_names = crate::pcb_lib::default_pin_names(&part.symbol, &part.value);
}

fn part_ref(app: &App, i: usize) -> String {
    match app.tab {
        crate::workspace::Tab::Pcb(pi) => app.ws.pcbs.get(pi).and_then(|p| p.parts.get(i)).map(|x| x.reference.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Whether the Place tool is placing a Digi-Key product (for the hint).
pub fn placing_name(app: &App) -> Option<String> {
    match (&app.pcb.tool, &app.pcb.dk.placing) {
        (Tool::Place(_), Some(p)) => Some(format!("{} {}", p.manufacturer, p.mpn).trim().to_string()),
        _ => None,
    }
}
