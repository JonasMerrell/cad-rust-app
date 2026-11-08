//! KiCad-style tools for the PCB tab: Annotate, the footprint assignment table (KiCad's
//! "Assign Footprints"), the properties window (E) and the hotkey list (?).

use std::time::Duration;

use egui::{RichText, Ui};

use crate::app::App;
use crate::pcb_annotate::{self, Order};
use crate::pcb_lib;
use crate::pcb_ui::View;
use crate::theme;

#[derive(Default)]
pub struct ToolsUi {
    pub annotate: bool,
    order: Order,
    /// Annotate keeps existing numbers.
    keep: bool,
    pub assign: bool,
    assign_filter: String,
    pub hotkeys: bool,
    pub props: bool,
}

fn index(app: &App) -> Option<usize> {
    match app.tab {
        crate::workspace::Tab::Pcb(i) if i < app.ws.pcbs.len() => Some(i),
        _ => None,
    }
}

pub fn windows(app: &mut App, ctx: &egui::Context) {
    annotate_window(app, ctx);
    assign_window(app, ctx);
    hotkeys_window(app, ctx);
    props_window(app, ctx);
}

/// The sidebar's Tools section.
pub fn section_ui(app: &mut App, ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Annotate…").on_hover_text("Number the references in order on the sheets (R1, R2, …)").clicked() {
            app.pcb.tools.annotate = true;
        }
        if ui.button("Assign footprints…").on_hover_text("Every part's footprint in one table, with KiCad's footprints that fit").clicked() {
            app.pcb.tools.assign = true;
        }
        if ui.button("Hotkeys  ?").on_hover_text("The keyboard shortcuts of this view (?)").clicked() {
            app.pcb.tools.hotkeys = true;
        }
    });
}

fn annotate_window(app: &mut App, ctx: &egui::Context) {
    if !app.pcb.tools.annotate {
        return;
    }
    let mut open = true;
    let mut run = false;
    egui::Window::new("Annotate").open(&mut open).resizable(false).default_width(300.0).show(ctx, |ui| {
        let t = &mut app.pcb.tools;
        ui.label(RichText::new("Order").strong());
        ui.radio_value(&mut t.order, Order::Columns, "Down each column, left to right");
        ui.radio_value(&mut t.order, Order::Rows, "Along each row, top to bottom");
        ui.add_space(4.0);
        ui.label(RichText::new("Numbers").strong());
        ui.radio_value(&mut t.keep, true, "Keep existing numbers, fill in R? and repeats");
        ui.radio_value(&mut t.keep, false, "Renumber every part");
        ui.add_space(6.0);
        run = ui.button(RichText::new("Annotate").strong()).clicked();
    });
    if run {
        let (order, keep) = (app.pcb.tools.order, app.pcb.tools.keep);
        let mut ch = vec![];
        crate::pcb_ui::edit(app, |p| ch = pcb_annotate::annotate(p, order, keep));
        let msg = if ch.is_empty() { "Every reference is already in order".to_string() } else { format!("Renamed {} part(s): {}", ch.len(), ch.iter().take(6).map(|(a, b)| format!("{a} → {b}")).collect::<Vec<_>>().join(", ")) };
        app.flash_info(msg, Some(Duration::from_secs(5)));
        app.pcb.tools.annotate = false;
    }
    if !open {
        app.pcb.tools.annotate = false;
    }
}

/// How many pins a part's symbol has, when its symbol fixes them (KiCad symbols do; the built-in
/// ones take their pins from the footprint).
fn symbol_pins(sym: &str) -> Option<usize> {
    let l = pcb_lib::lib_symbol(sym)?;
    Some(l.pins.iter().map(|p| p.num.as_str()).collect::<std::collections::BTreeSet<_>>().len())
}

fn assign_window(app: &mut App, ctx: &egui::Context) {
    if !app.pcb.tools.assign {
        return;
    }
    let Some(pi) = index(app) else { return };
    let mut open = true;
    let mut pick: Option<u64> = None;
    let mut set: Option<(usize, String)> = None;
    let fps = pcb_lib::footprint_names();
    egui::Window::new("Assign footprints").open(&mut open).default_size([640.0, 420.0]).show(ctx, |ui| {
        theme::field_style(ui);
        ui.add(egui::TextEdit::singleline(&mut app.pcb.tools.assign_filter).hint_text("Filter by reference, value or footprint").desired_width(260.0));
        ui.add_space(4.0);
        let q = app.pcb.tools.assign_filter.to_lowercase();
        let p = &app.ws.pcbs[pi];
        let mut order: Vec<usize> = (0..p.parts.len()).collect();
        order.sort_by(|a, b| crate::pcb::natural(&p.parts[*a].reference, &p.parts[*b].reference));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("assign-fp").striped(true).num_columns(5).spacing([12.0, 4.0]).show(ui, |ui| {
                for h in ["Ref", "Value", "Symbol", "Footprint", ""] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for i in order {
                    let part = &p.parts[i];
                    let line = format!("{} {} {}", part.reference, part.value, part.footprint).to_lowercase();
                    if !q.is_empty() && !line.contains(&q) {
                        continue;
                    }
                    ui.label(&part.reference);
                    ui.label(&part.value);
                    ui.label(RichText::new(part.symbol.rsplit(':').next().unwrap_or(&part.symbol)).color(theme::muted())).on_hover_text(&part.symbol);
                    let pads = part.footprint().pin_numbers().len();
                    let bad = symbol_pins(&part.symbol).is_some_and(|n| n != pads);
                    let mut fp = part.footprint.clone();
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("assign", part.id)).width(220.0).selected_text(RichText::new(&fp).size(12.0)).height(300.0).show_ui(ui, |ui| {
                            for f in &fps {
                                ui.selectable_value(&mut fp, f.clone(), f);
                            }
                        });
                        if bad {
                            ui.label(RichText::new("⚠").color(theme::error_soft())).on_hover_text(format!("{pads} pads for a symbol with {} pins", symbol_pins(&part.symbol).unwrap_or(0)));
                        }
                    });
                    if fp != part.footprint {
                        set = Some((i, fp));
                    }
                    if ui.small_button("KiCad…").on_hover_text("Choose from KiCad's footprints that fit this part").clicked() {
                        pick = Some(part.id);
                    }
                    ui.end_row();
                }
            });
        });
    });
    if let Some((i, fp)) = set {
        crate::pcb_ui::edit(app, |p| p.parts[i].footprint = fp);
    }
    if let Some(id) = pick {
        crate::kicad_lib_ui::open(app, crate::kicad_lib_ui::Mode::Footprint(id));
    }
    if !open {
        app.pcb.tools.assign = false;
    }
}

/// The keys of each view, as the hotkey list shows them.
pub fn hotkeys(view: View) -> Vec<(&'static str, &'static str)> {
    let common = [
        ("M / G", "Move the selection (or what's under the cursor) with the mouse; click to drop, Esc to undo"),
        ("E", "Properties of the selection (or what's under the cursor)"),
        ("R", "Rotate"),
        ("Del", "Delete"),
        ("Ctrl+D", "Duplicate the selected part and move it"),
        ("`", "Highlight the net under the cursor (again to clear)"),
        ("Home / F", "Zoom to fit"),
        ("B", "Switch between the schematic and the board"),
        ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
        ("?", "This list"),
    ];
    let mut v: Vec<(&str, &str)> = common.to_vec();
    match view {
        View::Board => v.extend([
            ("X", "Route a track"),
            ("V", "While routing: a via, carry on on the next layer"),
            ("L / + / −", "Next / previous copper layer"),
            ("PgUp / PgDn", "Top / bottom copper"),
            ("F", "Flip the selected part to the other side"),
        ]),
        _ => v.extend([
            ("A", "Add a symbol from the KiCad libraries"),
            ("Insert", "Place another of the last part"),
            ("W", "Wire"),
            ("L", "Net label"),
            ("G (tool off)", "Ground port; P sheet port"),
            ("Q", "No-connect flag"),
            ("X", "Mirror"),
            ("/", "Wire corner the other way"),
        ]),
    }
    v
}

fn hotkeys_window(app: &mut App, ctx: &egui::Context) {
    if !app.pcb.tools.hotkeys {
        return;
    }
    let mut open = true;
    let view = app.pcb.view;
    egui::Window::new(if view == View::Board { "Board hotkeys" } else { "Schematic hotkeys" }).open(&mut open).resizable(false).default_width(420.0).show(ctx, |ui| {
        egui::Grid::new("hotkeys").striped(true).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (k, what) in hotkeys(view) {
                ui.label(RichText::new(k).monospace().strong());
                ui.label(what);
                ui.end_row();
            }
        });
    });
    if !open {
        app.pcb.tools.hotkeys = false;
    }
}

fn props_window(app: &mut App, ctx: &egui::Context) {
    if !app.pcb.tools.props {
        return;
    }
    let Some(s) = app.pcb.sel else {
        app.pcb.tools.props = false;
        return;
    };
    let mut open = true;
    egui::Window::new("Properties").open(&mut open).default_width(320.0).default_pos([340.0, 120.0]).show(ctx, |ui| {
        // (called inside the sidebar's frame, so its edits get the sidebar's undo step)
        crate::pcb_ui::selection_ui(app, ui, s);
    });
    if !open {
        app.pcb.tools.props = false;
    }
}
