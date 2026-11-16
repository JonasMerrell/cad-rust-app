//! The KiCad library browser: KiCad's "Choose Symbol" and "Choose Footprint" dialogs in rCAD's
//! style. Search every official KiCad symbol by name, keyword or description, see it with its
//! footprint, pick another footprint from those its filters accept, and place it.

use std::collections::BTreeSet;
use std::time::Duration;

use egui::{RichText, Ui};

use crate::app::{App, ToastKind};
use crate::kicad_libs::{self as kl, Index, Shared};
use crate::pcb::{self, Pcb};
use crate::pcb_export::{self, BoardView, Draw};
use crate::pcb_lib;
use crate::theme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Add a symbol (A).
    #[default]
    Symbol,
    /// Add a power symbol (only power flags, GND, +5V…).
    Power,
    /// Pick a footprint for the part with this id.
    Footprint(u64),
}

#[derive(Default)]
pub struct KicadUi {
    pub open: bool,
    pub mode: Mode,
    query: String,
    fp_query: String,
    /// Show only footprints the symbol's filters and pin count accept.
    all_fps: bool,
    /// The chosen symbol and footprint ("Lib:Name"); no footprint means the symbol's own.
    sel: Option<String>,
    fp: Option<String>,
    /// Libraries opened in the tree (when nothing is searched).
    libs_open: BTreeSet<String>,
    job: Option<Shared>,
    /// Previews: (key, drawing or why there is none).
    sym_view: Option<(String, Result<Vec<Draw>, String>)>,
    fp_view: Option<(String, Result<(Vec<Draw>, bool), String>)>,
    focus: bool,
    /// List only parts from Digi-Key's library (they carry Digi-Key part numbers).
    dk_only: bool,
}

/// Open the browser (A in the schematic, "KiCad libraries…" in the Library section).
pub fn open(app: &mut App, mode: Mode) {
    let k = &mut app.pcb.kicad;
    k.open = true;
    k.focus = true;
    if k.mode != mode {
        k.mode = mode;
        k.fp = None;
        if let Mode::Footprint(_) = mode {
            k.fp_query.clear();
        }
    }
}

fn index_of(app: &App) -> Option<usize> {
    match app.tab {
        crate::workspace::Tab::Pcb(i) if i < app.ws.pcbs.len() => Some(i),
        _ => None,
    }
}

pub fn window(app: &mut App, ctx: &egui::Context) {
    if !app.pcb.kicad.open {
        return;
    }
    // a finished download or index build
    if let Some(job) = app.pcb.kicad.job.clone() {
        let g = job.lock().map(|g| g.clone()).unwrap_or_default();
        if g.finished {
            app.pcb.kicad.job = None;
            match g.error {
                Some(e) => app.flash(format!("KiCad libraries: {e}"), ToastKind::Error),
                None => {
                    let n = kl::index().map(|i| (i.symbols.len(), i.footprints.len())).unwrap_or_default();
                    app.flash_info(format!("KiCad libraries ready: {} symbols, {} footprints", n.0, n.1), Some(Duration::from_secs(5)));
                }
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
    let mut open = true;
    let title = match app.pcb.kicad.mode {
        Mode::Symbol => "Add symbol",
        Mode::Power => "Add power symbol",
        Mode::Footprint(_) => "Choose footprint",
    };
    let mut action: Option<Action> = None;
    egui::Window::new(title).id(egui::Id::new("kicad-libs")).open(&mut open).default_size([760.0, 560.0]).min_width(560.0).resizable(true).collapsible(false).show(ctx, |ui| {
        theme::field_style(ui);
        let root = kl::root();
        let ix = kl::index();
        match (root, ix) {
            (_, Some(ix)) => action = browser(app, ui, &ix),
            (root, None) => setup(app, ui, root.is_some()),
        }
    });
    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.pcb.kicad.open = false;
    }
    if let Some(a) = action {
        act(app, a);
    }
}

/// No index yet: download the libraries (or index an installed KiCad's).
fn setup(app: &mut App, ui: &mut Ui, have_root: bool) {
    ui.label(RichText::new("KiCad's community libraries").strong().size(15.0));
    ui.add_space(4.0);
    ui.label("KiCad's official libraries have about 23,000 symbols, 15,000 footprints and a 3D model for most of them, made and checked by the KiCad librarians. rCAD can search and place them here.");
    ui.add_space(6.0);
    if let Some(job) = &app.pcb.kicad.job {
        let g = job.lock().map(|g| g.clone()).unwrap_or_default();
        ui.add(egui::ProgressBar::new(g.done).text(g.stage).desired_width(ui.available_width()));
    } else if have_root {
        let label = kl::root().map(|r| r.label).unwrap_or_default();
        ui.label(RichText::new(format!("Found the {label}.")).color(theme::muted()));
        if ui.button("Index these libraries").on_hover_text("Reads every symbol and footprint once (a few seconds) so they can be searched").clicked() {
            app.pcb.kicad.job = Some(kl::start(false));
        }
        if ui.button(format!("Download KiCad {} instead", kl::VERSION)).clicked() {
            app.pcb.kicad.job = Some(kl::start(true));
        }
    } else {
        let where_ = kl::cache_dir().map(|d| d.display().to_string()).unwrap_or_default();
        if ui
            .add(egui::Button::new(RichText::new(format!("Download KiCad {} libraries (about 60 MB)", kl::VERSION)).strong()))
            .on_hover_text(format!("From gitlab.com/kicad/libraries into {where_}. 3D models are fetched one at a time when you make the 3D board."))
            .clicked()
        {
            app.pcb.kicad.job = Some(kl::start(true));
        }
        ui.label(RichText::new("With KiCad installed, rCAD uses its libraries instead (or set RCAD_KICAD_DIR to a folder with symbols/, footprints/ and 3dmodels/).").color(theme::muted()).size(11.5));
    }
    ui.add_space(10.0);
    licence(ui);
}

fn licence(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(kl::CREDIT).color(theme::muted()).size(11.5)).on_hover_text(kl::LICENSE);
        if ui.link(RichText::new("Licence").size(11.5)).on_hover_text(kl::LICENSE).clicked() {
            crate::printing::open_url(kl::LICENSE_URL);
        }
    });
    if kl::digikey_dir().is_some() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(kl::DK_CREDIT).color(theme::muted()).size(11.5)).on_hover_text(kl::DK_LICENSE);
            if ui.link(RichText::new("Licence").size(11.5)).on_hover_text(kl::DK_LICENSE).clicked() {
                crate::printing::open_url(&format!("{}/blob/master/LICENSE.md", kl::DK_HOME));
            }
        });
    }
}

/// Digi-Key's library: a filter once it is there, else a button to add it.
fn digikey_row(app: &mut App, ui: &mut Ui) {
    let k = &mut app.pcb.kicad;
    if kl::digikey_dir().is_some() {
        ui.checkbox(&mut k.dk_only, "Digi-Key parts only").on_hover_text("Parts from Digi-Key's KiCad library: each has its footprint and Digi-Key and manufacturer part numbers, which go on the part and into the BOM");
    } else if let Some(job) = &k.job {
        let g = job.lock().map(|g| g.clone()).unwrap_or_default();
        ui.add(egui::ProgressBar::new(g.done).text(g.stage).desired_width(300.0));
    } else if ui
        .button("Add Digi-Key's library…")
        .on_hover_text(format!("About 1,000 orderable parts from {}, each with its footprint and Digi-Key and manufacturer part numbers (about 10 MB, CC-BY-SA 4.0). Digi-Key no longer updates it, so check a part is still stocked.", kl::DK_HOME))
        .clicked()
    {
        k.job = Some(kl::start_digikey());
    }
}

enum Action {
    /// A power symbol: a supply or ground port with this net name.
    Power(String),
    /// Place symbol key with footprint key ("" for its own).
    Place(String, String),
    /// Give part id footprint key.
    SetFootprint(u64, String),
}

fn browser(app: &mut App, ui: &mut Ui, ix: &Index) -> Option<Action> {
    let mut action = None;
    let mode = app.pcb.kicad.mode;
    let part = match mode {
        Mode::Footprint(id) => index_of(app).and_then(|pi| app.ws.pcbs[pi].parts.iter().find(|p| p.id == id).cloned()),
        _ => None,
    };
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let h = ui.available_height().max(380.0) - 34.0;
    ui.horizontal_top(|ui| {
        // ---- left: search and results
        ui.vertical(|ui| {
            ui.set_width(300.0);
            let k = &mut app.pcb.kicad;
            let (q, hint) = if part.is_some() { (&mut k.fp_query, "Search footprints (name, tags, description)") } else { (&mut k.query, "Search (name, keyword, description, or Lib:Name)") };
            let r = ui.add(egui::TextEdit::singleline(q).hint_text(hint).desired_width(300.0));
            if k.focus {
                r.request_focus();
                k.focus = false;
            }
            if part.is_none() && mode != Mode::Power {
                digikey_row(app, ui);
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("kl-results").max_height(h - 30.0).auto_shrink([false, false]).show(ui, |ui| {
                if let Some(p) = &part {
                    footprint_list(app, ui, ix, p);
                } else {
                    symbol_list(app, ui, ix, mode == Mode::Power);
                }
            });
        });
        ui.separator();
        // ---- right: the chosen symbol and its footprint
        ui.vertical(|ui| {
            ui.set_min_width(380.0);
            egui::ScrollArea::vertical().id_salt("kl-detail").max_height(h).auto_shrink([false, false]).show(ui, |ui| {
                if let Some(p) = &part {
                    action = footprint_detail(app, ui, ix, p, enter);
                } else {
                    action = symbol_detail(app, ui, ix, enter);
                }
            });
        });
    });
    ui.separator();
    licence(ui);
    action
}

fn symbol_list(app: &mut App, ui: &mut Ui, ix: &Index, power: bool) {
    let k = &mut app.pcb.kicad;
    let dk_only = k.dk_only && !power && kl::digikey_dir().is_some();
    let shown = |s: &kl::SymEntry| !dk_only || s.from_digikey();
    let row = |ui: &mut Ui, s: &kl::SymEntry, on: bool| -> bool {
        let name = if s.digikey.is_empty() { s.name.clone() } else { format!("{} ({})", s.name, s.digikey) };
        let text = if s.desc.is_empty() { RichText::new(name).size(12.5) } else { RichText::new(format!("{name}  ·  {}", s.desc)).size(12.5) };
        ui.add(egui::Button::selectable(on, text).truncate()).on_hover_text(format!("{}\n{}", s.key(), s.desc)).clicked()
    };
    if k.query.trim().is_empty() {
        // the library tree, as KiCad shows it
        let mut libs: Vec<&str> = ix.symbols.iter().filter(|s| s.pins > 0 && (!power || s.power) && shown(s)).map(|s| s.lib.as_str()).collect();
        libs.dedup();
        for lib in libs {
            let open = k.libs_open.contains(lib) || power;
            let r = ui.add(egui::Button::selectable(false, RichText::new(format!("{} {lib}", if open { "▾" } else { "▸" })).strong().size(12.5)).frame(false));
            if r.clicked() && !power {
                if open {
                    k.libs_open.remove(lib);
                } else {
                    k.libs_open.insert(lib.to_string());
                }
            }
            if open {
                for s in ix.symbols.iter().filter(|s| s.lib == lib && s.pins > 0 && (!power || s.power)) {
                    let key = s.key();
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        if row(ui, s, k.sel.as_deref() == Some(&key)) {
                            k.sel = Some(key.clone());
                            k.fp = None;
                        }
                    });
                }
            }
        }
        return;
    }
    let hits: Vec<&kl::SymEntry> = if dk_only {
        kl::search_symbols(ix, &k.query, power, usize::MAX).into_iter().filter(|s| shown(s)).take(300).collect()
    } else {
        kl::search_symbols(ix, &k.query, power, 300)
    };
    if hits.is_empty() {
        ui.label(RichText::new("Nothing matches").color(theme::muted()));
    }
    // the best match is chosen while typing
    if k.sel.as_ref().is_none_or(|s| !hits.iter().any(|h| h.key() == *s)) {
        k.sel = hits.first().map(|h| h.key());
        k.fp = None;
    }
    for s in hits {
        let key = s.key();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&s.lib).color(theme::muted()).size(11.0));
            if row(ui, s, k.sel.as_deref() == Some(&key)) {
                k.sel = Some(key.clone());
                k.fp = None;
            }
        });
    }
}

/// Footprints for a symbol (filters `filters`, `pins` pads), searched with `q`.
fn fp_hits<'a>(ix: &'a Index, q: &str, filters: &[String], pins: u32, all: bool) -> Vec<&'a kl::FpEntry> {
    if all {
        kl::search_footprints(ix, q, &[], None, 300)
    } else {
        kl::search_footprints(ix, q, filters, Some(pins), 300)
    }
}

fn footprint_list(app: &mut App, ui: &mut Ui, ix: &Index, part: &pcb::Part) {
    let (filters, pins) = part_filters(ix, part);
    let k = &mut app.pcb.kicad;
    ui.checkbox(&mut k.all_fps, "Show all footprints").on_hover_text("Off: only footprints the symbol's filters accept, with one pad per pin");
    let hits = fp_hits(ix, &k.fp_query, &filters, pins, k.all_fps || filters.is_empty() && k.fp_query.trim().len() > 1);
    if hits.is_empty() {
        ui.label(RichText::new("Nothing matches: search, or show all footprints").color(theme::muted()));
    }
    if k.fp.as_ref().is_none_or(|s| !hits.iter().any(|h| h.key() == *s)) {
        k.fp = hits.first().map(|h| h.key());
    }
    for f in hits {
        let key = f.key();
        let on = k.fp.as_deref() == Some(&key);
        let r = ui.add(egui::Button::selectable(on, RichText::new(&f.name).size(12.5)).truncate()).on_hover_text(format!("{key}\n{}\n{} pads", f.desc, f.pads));
        if r.clicked() {
            k.fp = Some(key);
        }
    }
}

/// The filters and pin count a part's footprint should meet: its KiCad symbol's, or from its pins.
fn part_filters(ix: &Index, part: &pcb::Part) -> (Vec<String>, u32) {
    if let Some(s) = kl::find_symbol(ix, &part.symbol) {
        return (s.filters.clone(), s.pins);
    }
    let pins = part.footprint().pin_numbers().len() as u32;
    let f = match part.symbol.as_str() {
        "R" => vec!["R_*".to_string()],
        "C" => vec!["C_*".to_string(), "CP_*".to_string()],
        "L" => vec!["L_*".to_string()],
        "D" => vec!["D_*".to_string()],
        "LED" => vec!["LED_*".to_string()],
        _ => vec![],
    };
    (f, pins)
}

fn symbol_detail(app: &mut App, ui: &mut Ui, ix: &Index, enter: bool) -> Option<Action> {
    let key = app.pcb.kicad.sel.clone()?;
    let s = kl::find_symbol(ix, &key)?.clone();
    ui.label(RichText::new(&s.name).strong().size(16.0));
    ui.label(RichText::new(&s.lib).color(theme::muted()));
    if !s.desc.is_empty() {
        ui.label(&s.desc);
    }
    if !s.keys.is_empty() {
        ui.label(RichText::new(format!("Keywords: {}", s.keys)).color(theme::muted()).size(11.5));
    }
    if !s.digikey.is_empty() {
        let mut who = vec![format!("Digi-Key {}", s.digikey)];
        if !s.mpn.is_empty() {
            who.push(format!("MPN {}", s.mpn));
        }
        if !s.manufacturer.is_empty() {
            who.push(s.manufacturer.clone());
        }
        ui.label(RichText::new(who.join("  ·  ")).size(12.0)).on_hover_text("These go on the part when it is placed, and into the Digi-Key BOM");
    }
    ui.horizontal_wrapped(|ui| {
        if s.datasheet.starts_with("http") && ui.link(RichText::new("Datasheet").size(12.0)).on_hover_text(&s.datasheet).clicked() {
            crate::printing::open_url(&s.datasheet);
        }
        // the part on digikey.com: its own page for Digi-Key's parts, else a search
        let (label, q) = if s.digikey.is_empty() { ("Find on Digi-Key ↗", if s.lib == "Device" || s.lib.starts_with("power") { s.desc.clone() } else { s.name.clone() }) } else { ("Digi-Key ↗", s.digikey.clone()) };
        if ui.link(RichText::new(label).size(12.0)).on_hover_text(format!("Search digikey.com for \"{q}\"; import the results here with Digi-Key → Import CSV")).clicked() {
            crate::printing::open_url(&crate::digikey::web_search_url(&q));
        }
    });
    ui.add_space(4.0);
    // the symbol
    if app.pcb.kicad.sym_view.as_ref().is_none_or(|v| v.0 != key) {
        let v = kl::root().ok_or_else(|| "no libraries".to_string()).and_then(|root| kl::load_symbol(&root, ix, &s)).map(|sym| symbol_drawing(&sym));
        app.pcb.kicad.sym_view = Some((key.clone(), v));
    }
    match &app.pcb.kicad.sym_view.as_ref().unwrap().1 {
        Ok(d) => {
            crate::pcb_ui::preview(ui, d, egui::vec2(ui.available_width().min(420.0), 200.0), false);
        }
        Err(e) => {
            ui.colored_label(theme::error(), format!("Can't show this symbol: {e}"));
        }
    }
    ui.add_space(8.0);
    // its footprint: the symbol's own, or one picked from those its filters accept
    if s.power {
        ui.label(RichText::new("A power symbol: it names a net and has no footprint.").color(theme::muted()));
    } else {
        // no footprint of its own: the first one its filters accept, as placing would pick
        let own = if s.fp.is_empty() { kl::search_footprints(ix, "", &s.filters, Some(s.pins), 1).first().map(|f| f.key()) } else { Some(s.fp.clone()) };
        let own_tag = if s.fp.is_empty() { "first that fits" } else { "the symbol's own" };
        let chosen = app.pcb.kicad.fp.clone().or(own.clone());
        ui.horizontal(|ui| {
            ui.label(RichText::new("Footprint").strong());
            let k = &mut app.pcb.kicad;
            let text = chosen.clone().unwrap_or_else(|| "pick one".into());
            egui::ComboBox::from_id_salt("kl-fp").selected_text(RichText::new(text).size(12.0)).width(290.0).height(360.0).show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut k.fp_query).hint_text("Search footprints").desired_width(280.0));
                ui.checkbox(&mut k.all_fps, "Show all footprints");
                if let Some(o) = &own {
                    if ui.selectable_label(k.fp.is_none(), format!("{o} ({own_tag})")).clicked() {
                        k.fp = None;
                    }
                }
                for f in fp_hits(ix, &k.fp_query, &s.filters, s.pins, k.all_fps).into_iter().take(150) {
                    let fk = f.key();
                    if ui.selectable_label(k.fp.as_deref() == Some(&fk), &fk).on_hover_text(&f.desc).clicked() {
                        k.fp = Some(fk);
                    }
                }
            });
        });
        if !s.filters.is_empty() {
            ui.label(RichText::new(format!("Fits: {}", s.filters.join("  "))).color(theme::muted()).size(11.0));
        }
        if let Some(fk) = &chosen {
            footprint_preview(app, ui, ix, fk);
        }
    }
    ui.add_space(8.0);
    let place = ui.add(egui::Button::new(RichText::new("Place  ⏎").strong())).on_hover_text("Load it into this PCB and place it on the schematic");
    if (place.clicked() || enter) && s.power {
        let v = kl::root().and_then(|root| kl::load_symbol(&root, ix, &s).ok()).map(|x| x.value).unwrap_or_else(|| s.name.clone());
        return Some(Action::Power(v));
    }
    if place.clicked() || enter {
        let fp = app.pcb.kicad.fp.clone().or(if s.power || !s.fp.is_empty() { None } else { kl::search_footprints(ix, "", &s.filters, Some(s.pins), 1).first().map(|f| f.key()) });
        return Some(Action::Place(key, fp.unwrap_or_default()));
    }
    None
}

fn footprint_detail(app: &mut App, ui: &mut Ui, ix: &Index, part: &pcb::Part, enter: bool) -> Option<Action> {
    ui.label(RichText::new(format!("{} {}", part.reference, part.value)).strong().size(16.0));
    ui.label(RichText::new(format!("Now: {}", part.footprint)).color(theme::muted()));
    let fk = app.pcb.kicad.fp.clone()?;
    let f = kl::find_footprint(ix, &fk)?.clone();
    ui.add_space(4.0);
    ui.label(RichText::new(&f.name).strong());
    if !f.desc.is_empty() {
        ui.label(&f.desc);
    }
    footprint_preview(app, ui, ix, &fk);
    let pins = part.footprint().pin_numbers().len() as u32;
    if f.pads != pins {
        ui.colored_label(theme::error_soft(), format!("{} pads for a part with {pins} pins: the pins left over stay unconnected", f.pads));
    }
    ui.add_space(8.0);
    if ui.add(egui::Button::new(RichText::new("Use this footprint  ⏎").strong())).clicked() || enter {
        return Some(Action::SetFootprint(part.id, fk));
    }
    None
}

fn footprint_preview(app: &mut App, ui: &mut Ui, ix: &Index, fk: &str) {
    if app.pcb.kicad.fp_view.as_ref().is_none_or(|v| v.0 != fk) {
        let v = match (kl::root(), kl::find_footprint(ix, fk)) {
            (Some(root), Some(f)) => kl::load_footprint(&root, f).map(|fp| (footprint_drawing(&fp), fp.model.is_some())),
            _ => Err(format!("{fk} is not in the libraries")),
        };
        app.pcb.kicad.fp_view = Some((fk.to_string(), v));
    }
    match &app.pcb.kicad.fp_view.as_ref().unwrap().1 {
        Ok((d, model)) => {
            crate::pcb_ui::preview(ui, d, egui::vec2(ui.available_width().min(420.0), 170.0), true);
            ui.label(RichText::new(if *model { "Has a KiCad 3D model (STEP), used for the 3D board" } else { "No 3D model: the 3D board shows a box" }).color(theme::muted()).size(11.5));
        }
        Err(e) => {
            ui.colored_label(theme::error(), e);
        }
    }
}

fn symbol_drawing(sym: &pcb_lib::LibSymbol) -> Vec<Draw> {
    pcb_lib::register_symbols(std::slice::from_ref(sym));
    let mut p = Pcb::default();
    p.parts.push(pcb::Part { id: 1, reference: format!("{}?", sym.prefix), value: sym.value.clone(), symbol: sym.key.clone(), footprint: "R_0805".into(), ..Default::default() });
    let nl = p.netlist();
    pcb_export::schematic_scene(&p, &nl, None)
}

fn footprint_drawing(fp: &pcb_lib::Footprint) -> Vec<Draw> {
    pcb_lib::register_footprints(std::slice::from_ref(fp));
    let mut p = Pcb::default();
    p.board.outline.clear();
    p.parts.push(pcb::Part { id: 1, reference: String::new(), symbol: "R".into(), footprint: fp.name.clone(), ..Default::default() });
    let nl = p.netlist();
    let pads = p.pads(&nl);
    pcb_export::board_scene(&p, &nl, &pads, &[], &[], &BoardView::default())
}

fn act(app: &mut App, a: Action) {
    match a {
        Action::Power(name) => {
            // rCAD's supply and ground ports are KiCad's power symbols: a port names its net
            let ground = { let u = name.to_uppercase(); u.contains("GND") || u.contains("EARTH") || u.starts_with("VSS") || u == "CHASSIS" };
            let kind = if ground { pcb::LabelKind::Ground } else { pcb::LabelKind::Power };
            app.pcb.kicad.open = false;
            crate::pcb_ui::start_label(app, kind, &name);
            app.flash_info(format!("Click to place {name} ports: they join the {name} net · Esc stops"), Some(Duration::from_secs(5)));
        }
        Action::Place(key, fp) => {
            let mut res = Ok(vec![]);
            crate::pcb_ui::edit(app, |p| res = kl::add_symbol(p, &key, &fp));
            match res {
                Ok(notes) => {
                    app.pcb.kicad.open = false;
                    crate::pcb_ui::start_place(app, &key);
                    let name = key.rsplit(':').next().unwrap_or(&key).to_string();
                    let mut msg = format!("Click to place {name} · R turns it, X mirrors it, Esc stops");
                    if !notes.is_empty() {
                        msg += &format!(" · {}", notes.join(" · "));
                    }
                    app.flash_info(msg, Some(Duration::from_secs(6)));
                }
                Err(e) => app.flash(format!("Could not load {key}: {e}"), ToastKind::Error),
            }
        }
        Action::SetFootprint(id, fp) => {
            let mut res = Err(String::new());
            crate::pcb_ui::edit(app, |p| {
                res = match p.parts.iter().position(|x| x.id == id) {
                    Some(i) => kl::set_footprint(p, i, &fp),
                    None => Err("the part is gone".into()),
                }
            });
            match res {
                Ok(name) => {
                    app.pcb.kicad.open = false;
                    app.flash_info(format!("Footprint {name}"), Some(Duration::from_secs(3)));
                }
                Err(e) => app.flash(format!("Could not use {fp}: {e}"), ToastKind::Error),
            }
        }
    }
}
