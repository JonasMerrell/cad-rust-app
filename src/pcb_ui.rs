//! The PCB tab: a schematic editor and a board editor sharing one set of parts.

use std::time::{Duration, Instant};

use egui::{Color32, Pos2, RichText, Sense, Stroke, Ui};

use crate::app::{App, ToastKind};
use crate::cam_ui::{row, section};
use crate::pcb::{self, Hole, Issue, Label, LabelKind, Layer, Netlist, Pcb, Track, Via, WPad, Wire, Zone};
use crate::pcb_export::{self, BoardView, Draw, Prim};
use crate::pcb_lib::{self, GRID, P};
use crate::theme;
use crate::workspace::Tab;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Schematic,
    Board,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Tool {
    #[default]
    Select,
    /// Placing a library part (symbol key).
    Place(String),
    Wire,
    Label(LabelKind),
    Route,
    Zone,
    Hole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sel {
    Part(usize),
    Wire(usize),
    Label(usize),
    Track(usize),
    Via(usize),
    Zone(usize),
    Hole(usize),
}

struct Drag {
    /// Where the drag started, in world mm.
    from: P,
    /// The part's position then.
    orig: P,
    /// Wire ends attached to the part's pins: (wire, end b?, original point).
    ends: Vec<(usize, bool, P)>,
}

struct Route {
    net: String,
    layer: Layer,
    pts: Vec<P>,
    width: f64,
}

/// Derived data, rebuilt when the PCB changes.
#[derive(Default)]
struct Cache {
    pcb: Option<usize>,
    nl: Netlist,
    pads: Vec<WPad>,
    fills: Vec<pcb::Fill>,
    air: Vec<pcb::Airwire>,
}

pub struct PcbUi {
    pub view: View,
    pub tool: Tool,
    /// Per view: millimetres per point... pixels per mm and the centre.
    zoom: [f32; 2],
    center: [P; 2],
    fitted: [bool; 2],
    pub sel: Option<Sel>,
    drag: Option<Drag>,
    /// Rotation and mirror for the part being placed.
    place_rot: u8,
    place_mirror: bool,
    wire: Vec<P>,
    /// Draw the wire preview vertical first.
    wire_vfirst: bool,
    route: Option<Route>,
    zone_pts: Vec<P>,
    pub layer: Layer,
    show: BoardView,
    undo: Vec<Pcb>,
    redo: Vec<Pcb>,
    last_ck: Option<Instant>,
    dirty: bool,
    cache: Cache,
    pub issues: Option<Vec<Issue>>,
    lib_search: String,
    label_text: String,
    zone_net: String,
    highlight: Option<String>,
    hover: P,
    pin_text: String,
    pin_text_for: Option<u64>,
}

impl Default for PcbUi {
    fn default() -> Self {
        PcbUi {
            view: View::Schematic,
            tool: Tool::Select,
            zoom: [0.0; 2],
            center: [[0.0; 2]; 2],
            fitted: [false; 2],
            sel: None,
            drag: None,
            place_rot: 0,
            place_mirror: false,
            wire: vec![],
            wire_vfirst: false,
            route: None,
            zone_pts: vec![],
            layer: Layer::Top,
            show: BoardView::default(),
            undo: vec![],
            redo: vec![],
            last_ck: None,
            dirty: true,
            cache: Cache::default(),
            issues: None,
            lib_search: String::new(),
            label_text: String::new(),
            zone_net: String::new(),
            highlight: None,
            hover: [0.0; 2],
            pin_text: String::new(),
            pin_text_for: None,
        }
    }
}

fn index(app: &App) -> Option<usize> {
    match app.tab {
        Tab::Pcb(i) if i < app.ws.pcbs.len() => Some(i),
        _ => None,
    }
}

fn pcb_ref(app: &App) -> Option<&Pcb> {
    index(app).map(|i| &app.ws.pcbs[i])
}

pub fn undo(app: &mut App) {
    let Some(i) = index(app) else { return };
    if let Some(c) = app.pcb.undo.pop() {
        let cur = std::mem::replace(&mut app.ws.pcbs[i], c);
        app.pcb.redo.push(cur);
        after_history(app);
    }
}

pub fn redo(app: &mut App) {
    let Some(i) = index(app) else { return };
    if let Some(c) = app.pcb.redo.pop() {
        let cur = std::mem::replace(&mut app.ws.pcbs[i], c);
        app.pcb.undo.push(cur);
        after_history(app);
    }
}

fn after_history(app: &mut App) {
    app.pcb.dirty = true;
    app.pcb.sel = None;
    app.pcb.drag = None;
    app.pcb.route = None;
    app.pcb.wire.clear();
    app.pcb.issues = None;
}

pub fn can_undo(app: &App) -> bool {
    !app.pcb.undo.is_empty()
}

pub fn can_redo(app: &App) -> bool {
    !app.pcb.redo.is_empty()
}

/// Record an undo step with the state before an edit (edits closer together than a second share one).
fn record(app: &mut App, before: Pcb) {
    let now = Instant::now();
    if app.pcb.last_ck.map(|t| now.duration_since(t).as_secs_f32() > 1.0).unwrap_or(true) {
        app.pcb.undo.push(before);
        if app.pcb.undo.len() > 200 {
            app.pcb.undo.remove(0);
        }
        app.pcb.redo.clear();
    }
    app.pcb.last_ck = Some(now);
    app.pcb.dirty = true;
    app.pcb.issues = None;
}

/// Make an edit as its own undo step.
fn edit(app: &mut App, f: impl FnOnce(&mut Pcb)) {
    let Some(i) = index(app) else { return };
    let before = app.ws.pcbs[i].clone();
    f(&mut app.ws.pcbs[i]);
    if app.ws.pcbs[i] != before {
        app.pcb.last_ck = None;
        record(app, before);
        app.pcb.last_ck = None;
    }
}

fn refresh(app: &mut App) {
    let Some(i) = index(app) else { return };
    if !app.pcb.dirty && app.pcb.cache.pcb == Some(i) {
        return;
    }
    let p = &app.ws.pcbs[i];
    let nl = p.netlist();
    let pads = p.pads(&nl);
    // pours are slow to refill: keep the old fill while a part is being dragged
    let fills = if app.pcb.drag.is_some() && app.pcb.cache.pcb == Some(i) && app.pcb.cache.fills.len() == p.board.zones.len() {
        std::mem::take(&mut app.pcb.cache.fills)
    } else {
        pcb::fill_zones(p, &nl, &pads)
    };
    let its = pcb::items(p, &nl, &pads, &fills);
    let air = pcb::ratsnest(&its, &pads, &fills);
    app.pcb.cache = Cache { pcb: Some(i), nl, pads, fills, air };
    app.pcb.dirty = false;
}

fn vi(v: View) -> usize {
    match v {
        View::Schematic => 0,
        View::Board => 1,
    }
}

pub fn set_view(app: &mut App, v: View) {
    app.pcb.view = v;
    app.pcb.tool = Tool::Select;
    app.pcb.sel = None;
    app.pcb.wire.clear();
    app.pcb.route = None;
    app.pcb.zone_pts.clear();
}

pub fn fit(app: &mut App) {
    let k = vi(app.pcb.view);
    app.pcb.fitted[k] = false;
}

fn set_tool(app: &mut App, t: Tool) {
    app.pcb.tool = t;
    app.pcb.wire.clear();
    app.pcb.route = None;
    app.pcb.zone_pts.clear();
    app.pcb.drag = None;
}

// ---------------------------------------------------------------- commands

pub(crate) fn autoroute(app: &mut App) {
    let Some(i) = index(app) else { return };
    let start = Instant::now();
    let mut out = None;
    edit(app, |p| out = Some(crate::pcb_route::autoroute(p, None)));
    let Some(out) = out else { return };
    let secs = start.elapsed().as_secs_f32();
    let _ = i;
    if out.routed == 0 && out.failed.is_empty() {
        app.flash_info("Nothing to route: every connection is made", Some(Duration::from_secs(3)));
    } else if out.failed.is_empty() {
        app.flash_info(format!("Routed {} connections ({} tracks, {} vias) in {secs:.1} s", out.routed, out.tracks, out.vias), Some(Duration::from_secs(4)));
    } else {
        app.flash(format!("Routed {}; no way found for {}. Move parts apart or route those by hand", out.routed, out.failed.join(", ")), ToastKind::Error);
    }
}

pub(crate) fn auto_place(app: &mut App) {
    let mut n = 0;
    edit(app, |p| n = crate::pcb_route::auto_place(p, false));
    if n == 0 {
        app.flash_info("Every part is already on the board (drag them, or use Place all to start over)", Some(Duration::from_secs(3)));
    } else {
        app.flash_info(format!("Placed {n} parts on the board"), Some(Duration::from_secs(3)));
    }
    if app.pcb.view != View::Board {
        set_view(app, View::Board);
    }
}

pub(crate) fn run_drc(app: &mut App) {
    refresh(app);
    let Some(p) = pcb_ref(app) else { return };
    let c = &app.pcb.cache;
    let issues = pcb::drc(p, &c.nl, &c.pads, &c.fills);
    let errors = issues.iter().filter(|i| i.error).count();
    if issues.is_empty() {
        app.flash_info("Design rule check passed: no problems found", Some(Duration::from_secs(3)));
    } else {
        app.flash_info(format!("Design rule check: {errors} errors, {} warnings (listed in the sidebar)", issues.len() - errors), Some(Duration::from_secs(4)));
    }
    app.pcb.issues = Some(issues);
    if app.pcb.view != View::Board {
        set_view(app, View::Board);
    }
}

pub(crate) fn export_fab(app: &mut App) {
    let Some(p) = pcb_ref(app).cloned() else { return };
    let name = format!("{}.zip", p.name.replace(['/', '\\'], "_"));
    let Some(path) = app.save_file_dialog("Gerber ZIP", &["zip"], name) else { return };
    match pcb_export::export(&p, &path) {
        Ok(files) => app.flash_info(format!("Saved {}", files.join(", ")), Some(Duration::from_secs(5))),
        Err(e) => app.flash(format!("Could not export: {e}"), ToastKind::Error),
    }
}

pub(crate) fn to_3d(app: &mut App) {
    let Some(i) = index(app) else { return };
    app.sync_ws();
    let n = app.ws.studios.len();
    match pcb_export::to_studio(&mut app.ws, i) {
        Ok(si) => {
            if si == app.cur_studio {
                app.checkpoint();
                app.doc = app.ws.studios[si].doc.clone();
                app.changed();
            } else {
                app.studio_results.remove(&si);
            }
            app.switch_tab(Tab::Studio(si));
            app.changed();
            app.first_fit = true;
            let what = if app.ws.studios.len() > n { "Made" } else { "Updated" };
            app.flash_info(format!("{what} Part Studio “{}”: insert it into an assembly or export STEP", app.ws.studios[si].name), Some(Duration::from_secs(4)));
        }
        Err(e) => app.flash(e, ToastKind::Error),
    }
}

fn pour(app: &mut App, net: String) {
    let layer = app.pcb.layer;
    edit(app, |p| {
        let outline = p.board.outline.clone();
        p.board.zones.retain(|z| !(z.layer == layer && z.net == net));
        p.board.zones.push(Zone { net, layer, outline });
    });
}

fn delete_sel(app: &mut App) {
    let Some(s) = app.pcb.sel.take() else { return };
    edit(app, |p| match s {
        Sel::Part(i) if i < p.parts.len() => {
            p.parts.remove(i);
        }
        Sel::Wire(i) if i < p.wires.len() => {
            p.wires.remove(i);
        }
        Sel::Label(i) if i < p.labels.len() => {
            p.labels.remove(i);
        }
        Sel::Track(i) if i < p.board.tracks.len() => {
            p.board.tracks.remove(i);
        }
        Sel::Via(i) if i < p.board.vias.len() => {
            p.board.vias.remove(i);
        }
        Sel::Zone(i) if i < p.board.zones.len() => {
            p.board.zones.remove(i);
        }
        Sel::Hole(i) if i < p.board.holes.len() => {
            p.board.holes.remove(i);
        }
        _ => {}
    });
}

fn rotate_sel(app: &mut App) {
    if let Tool::Place(_) = app.pcb.tool {
        app.pcb.place_rot = (app.pcb.place_rot + 1) % 4;
        return;
    }
    let view = app.pcb.view;
    match app.pcb.sel {
        Some(Sel::Part(i)) => edit(app, |p| {
            if view == View::Schematic {
                p.parts[i].sch_rot = (p.parts[i].sch_rot + 1) % 4;
            } else {
                p.parts[i].rot = (p.parts[i].rot + 90.0).rem_euclid(360.0);
            }
        }),
        Some(Sel::Label(i)) => edit(app, |p| p.labels[i].rot = (p.labels[i].rot + 1) % 4),
        _ => {}
    }
}

fn mirror_sel(app: &mut App) {
    if let Tool::Place(_) = app.pcb.tool {
        app.pcb.place_mirror = !app.pcb.place_mirror;
        return;
    }
    let view = app.pcb.view;
    if let Some(Sel::Part(i)) = app.pcb.sel {
        edit(app, |p| {
            if view == View::Schematic {
                p.parts[i].sch_mirror = !p.parts[i].sch_mirror;
            } else {
                p.parts[i].bottom = !p.parts[i].bottom;
            }
        });
    }
}

pub fn hint(app: &App) -> String {
    match (&app.pcb.view, &app.pcb.tool) {
        (_, Tool::Place(k)) => format!("Click to place {} · R rotates, X mirrors · Esc when done", pcb_lib::symbol_kind(k).map(|s| s.name).unwrap_or("the part")),
        (_, Tool::Wire) if app.pcb.wire.is_empty() => "Click a pin or wire end to start a wire · Esc stops".into(),
        (_, Tool::Wire) => "Click to add a corner, click a pin or wire to finish · / flips the corner · Esc or double-click ends".into(),
        (_, Tool::Label(_)) => "Click a pin or wire to name its net · the text is in the sidebar".into(),
        (_, Tool::Route) if app.pcb.route.is_none() => "Click a pad or track to start routing its net".into(),
        (_, Tool::Route) => "Click to add a corner, click the target pad to finish · V adds a via and changes layer · Esc ends".into(),
        (_, Tool::Zone) => "Click the corners of the pour, double-click or Enter to close it · or use Pour in the sidebar".into(),
        (_, Tool::Hole) => "Click to add a mounting hole".into(),
        (View::Schematic, _) => "Add parts from the library, wire their pins (W), name nets (L) · B switches to the board".into(),
        (View::Board, _) => "Drag parts onto the board (R rotates, F flips), then route (X) or autoroute · L changes layer · B back to the schematic".into(),
    }
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    let (ctrl, _shift) = crate::app::key_mods(ctx);
    let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
    if ctrl {
        return;
    }
    let board = app.pcb.view == View::Board;
    if pressed(egui::Key::Escape) {
        if app.pcb.route.is_some() || !app.pcb.wire.is_empty() || !app.pcb.zone_pts.is_empty() {
            app.pcb.route = None;
            app.pcb.wire.clear();
            app.pcb.zone_pts.clear();
        } else if app.pcb.tool != Tool::Select {
            set_tool(app, Tool::Select);
        } else {
            app.pcb.sel = None;
            app.pcb.highlight = None;
        }
    }
    if pressed(egui::Key::Enter) && app.pcb.tool == Tool::Zone {
        close_zone(app);
    }
    if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
        delete_sel(app);
    }
    if pressed(egui::Key::R) {
        rotate_sel(app);
    }
    if pressed(egui::Key::X) && !board {
        mirror_sel(app);
    }
    if pressed(egui::Key::F) {
        if board && matches!(app.pcb.sel, Some(Sel::Part(_))) {
            mirror_sel(app);
        } else {
            fit(app);
        }
    }
    if pressed(egui::Key::B) {
        set_view(app, if board { View::Schematic } else { View::Board });
    }
    if !board {
        if pressed(egui::Key::W) {
            set_tool(app, Tool::Wire);
        }
        if pressed(egui::Key::L) {
            set_tool(app, Tool::Label(LabelKind::Net));
        }
        if pressed(egui::Key::G) {
            set_tool(app, Tool::Label(LabelKind::Ground));
        }
        if pressed(egui::Key::Slash) {
            app.pcb.wire_vfirst = !app.pcb.wire_vfirst;
        }
    } else {
        if pressed(egui::Key::X) {
            set_tool(app, Tool::Route);
        }
        if pressed(egui::Key::L) {
            app.pcb.layer = app.pcb.layer.other();
            if let Some(r) = &mut app.pcb.route {
                r.layer = app.pcb.layer;
            }
        }
        if pressed(egui::Key::V) {
            via_here(app);
        }
    }
}

/// While routing: a via at the last point, and carry on on the other layer.
fn via_here(app: &mut App) {
    let Some(r) = &app.pcb.route else { return };
    let at = *r.pts.last().unwrap();
    let net = r.net.clone();
    let layer = r.layer.other();
    edit(app, |p| {
        let (d, drill) = (p.board.rules.via_d, p.board.rules.via_drill);
        if !p.board.vias.iter().any(|v| pcb::dist(v.at, at) < 1e-6) {
            p.board.vias.push(Via { at, d, drill, net });
        }
    });
    app.pcb.layer = layer;
    if let Some(r) = &mut app.pcb.route {
        r.layer = layer;
        r.pts = vec![at];
    }
}

// ---------------------------------------------------------------- toolbar

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    let board = app.pcb.view == View::Board;
    App::group(ui, |ui| {
        if app.tool_button(ui, "pcbSchematic", "Schematic", "The circuit: parts, wires and net labels (B)", true, !board, true).clicked() {
            set_view(app, View::Schematic);
        }
        if app.tool_button(ui, "pcbBoard", "Board", "The layout: footprints, tracks and copper (B)", true, board, true).clicked() {
            set_view(app, View::Board);
        }
    });
    let t = app.pcb.tool.clone();
    if !board {
        App::group(ui, |ui| {
            if app.tool_button(ui, "select", "Select", "Select and drag (Esc)", false, t == Tool::Select, true).clicked() {
                set_tool(app, Tool::Select);
            }
            if app.tool_button(ui, "pcbWire", "Wire", "Connect pins with wires (W)", true, t == Tool::Wire, true).clicked() {
                set_tool(app, Tool::Wire);
            }
            if app.tool_button(ui, "pcbLabel", "Net label", "Name a net; pins with the same label are connected (L)", false, t == Tool::Label(LabelKind::Net), true).clicked() {
                set_tool(app, Tool::Label(LabelKind::Net));
            }
            if app.tool_button(ui, "pcbPower", "Power", "A supply port such as VCC or +3V3", false, t == Tool::Label(LabelKind::Power), true).clicked() {
                if app.pcb.label_text.is_empty() || app.pcb.label_text == "GND" {
                    app.pcb.label_text = "VCC".into();
                }
                set_tool(app, Tool::Label(LabelKind::Power));
            }
            if app.tool_button(ui, "pcbGround", "Ground", "A ground port (G)", false, t == Tool::Label(LabelKind::Ground), true).clicked() {
                set_tool(app, Tool::Label(LabelKind::Ground));
            }
        });
        App::group(ui, |ui| {
            if app.tool_button(ui, "pcbRotate", "Rotate", "Rotate the selection (R)", false, false, app.pcb.sel.is_some()).clicked() {
                rotate_sel(app);
            }
            if app.tool_button(ui, "mirror", "Mirror", "Mirror the selected part (X)", false, false, matches!(app.pcb.sel, Some(Sel::Part(_)))).clicked() {
                mirror_sel(app);
            }
            if app.tool_button(ui, "delete", "Delete", "Delete the selection (Del)", false, false, app.pcb.sel.is_some()).clicked() {
                delete_sel(app);
            }
        });
    } else {
        App::group(ui, |ui| {
            if app.tool_button(ui, "select", "Select", "Select and drag (Esc)", false, t == Tool::Select, true).clicked() {
                set_tool(app, Tool::Select);
            }
            if app.tool_button(ui, "pcbRoute", "Route", "Draw tracks from pad to pad (X); V adds a via", true, t == Tool::Route, true).clicked() {
                set_tool(app, Tool::Route);
            }
            if app.tool_button(ui, "pcbZone", "Pour", "Draw a copper pour (zone) for a net", false, t == Tool::Zone, true).clicked() {
                set_tool(app, Tool::Zone);
            }
            if app.tool_button(ui, "pcbHole", "Hole", "Add a mounting hole", false, t == Tool::Hole, true).clicked() {
                set_tool(app, Tool::Hole);
            }
        });
        App::group(ui, |ui| {
            let fg = |on: bool| if on { theme::active_text() } else { theme::text() };
            for l in [Layer::Top, Layer::Bottom] {
                let on = app.pcb.layer == l;
                let col = if l == Layer::Top { pcb_export::colors::TOP } else { pcb_export::colors::BOTTOM };
                let mut b = egui::Button::new(RichText::new(format!("● {}", l.label())).color(if on { Color32::from_rgb(col[0], col[1], col[2]) } else { fg(false) }).size(12.5)).min_size(egui::vec2(26.0, 26.0));
                if on {
                    b = b.fill(theme::active_fill()).stroke(Stroke::new(1.0, theme::active_stroke()));
                }
                if ui.add(b).on_hover_text("The copper layer new tracks go on (L)").clicked() {
                    app.pcb.layer = l;
                }
            }
        });
        App::group(ui, |ui| {
            if app.tool_button(ui, "pcbRotate", "Rotate", "Rotate the selected part 90° (R)", false, false, matches!(app.pcb.sel, Some(Sel::Part(_)))).clicked() {
                rotate_sel(app);
            }
            if app.tool_button(ui, "pcbFlip", "Flip", "Put the selected part on the other side (F)", false, false, matches!(app.pcb.sel, Some(Sel::Part(_)))).clicked() {
                mirror_sel(app);
            }
            if app.tool_button(ui, "delete", "Delete", "Delete the selection (Del)", false, false, app.pcb.sel.is_some()).clicked() {
                delete_sel(app);
            }
        });
        App::group(ui, |ui| {
            if app.tool_button(ui, "pcbPlace", "Place", "Put parts that are off the board onto it", false, false, true).clicked() {
                auto_place(app);
            }
            if app.tool_button(ui, "pcbAutoroute", "Autoroute", "Route every unrouted connection", true, false, true).clicked() {
                autoroute(app);
            }
            if app.tool_button(ui, "pcbDrc", "Check", "Design rule check: clearances, shorts, unrouted nets", true, app.pcb.issues.is_some(), true).clicked() {
                run_drc(app);
            }
        });
    }
    App::group(ui, |ui| {
        if app.tool_button(ui, "export", "Gerbers", "Save the Gerber and drill ZIP for a board maker, with BOM and pick-and-place files", false, false, true).clicked() {
            export_fab(app);
        }
        if app.tool_button(ui, "cube", "3D board", "Make (or update) a Part Studio with the board and its parts, for assemblies and STEP", false, false, true).clicked() {
            to_3d(app);
        }
    });
}

// ---------------------------------------------------------------- sidebar

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    let Some(i) = index(app) else {
        ui.label("No PCB tab");
        return;
    };
    refresh(app);
    let before = app.ws.pcbs[i].clone();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if app.pcb.view == View::Schematic {
            section(ui, "Library", |ui| library_ui(app, ui));
            if matches!(app.pcb.tool, Tool::Label(_)) {
                section(ui, "Label", |ui| {
                    row(ui, "Net name", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut app.pcb.label_text).desired_width(120.0).hint_text(default_label(&app.pcb.tool)));
                    });
                });
            }
        }
        if let Some(s) = app.pcb.sel {
            section(ui, "Selection", |ui| selection_ui(app, ui, s));
        }
        if app.pcb.view == View::Board {
            section(ui, "Board", |ui| board_ui(app, ui));
            section(ui, "Layers", |ui| layers_ui(app, ui));
            if let Some(issues) = app.pcb.issues.clone() {
                section(ui, "Check", |ui| issues_ui(app, ui, &issues));
            }
        }
        section(ui, "Nets", |ui| nets_ui(app, ui));
    });
    // edits made through edit() have their own undo step
    if app.ws.pcbs[i] != before && app.pcb.drag.is_none() && app.pcb.undo.last() != Some(&before) {
        record(app, before);
    }
}

fn default_label(t: &Tool) -> &'static str {
    match t {
        Tool::Label(LabelKind::Ground) => "GND",
        Tool::Label(LabelKind::Power) => "VCC",
        _ => "NET1",
    }
}

fn library_ui(app: &mut App, ui: &mut Ui) {
    theme::field_style(ui);
    ui.add(egui::TextEdit::singleline(&mut app.pcb.lib_search).hint_text("Search parts").desired_width(ui.available_width()));
    ui.add_space(4.0);
    let q = app.pcb.lib_search.to_lowercase();
    for s in pcb_lib::SYMBOLS {
        if !q.is_empty() && !(s.name.to_lowercase().contains(&q) || s.tags.contains(&q) || s.key.to_lowercase() == q) {
            continue;
        }
        let on = app.pcb.tool == Tool::Place(s.key.to_string());
        let r = ui.add_sized([ui.available_width(), 22.0], egui::Button::selectable(on, RichText::new(format!("{}  ·  {} {}", s.name, s.footprint, s.value)).size(12.5)));
        if r.on_hover_text(format!("Place a {} ({}); the footprint and value can be changed after", s.name.to_lowercase(), s.prefix)).clicked() {
            set_tool(app, Tool::Place(s.key.to_string()));
            app.pcb.place_rot = 0;
            app.pcb.place_mirror = false;
        }
    }
}

fn selection_ui(app: &mut App, ui: &mut Ui, s: Sel) {
    let Some(pi) = index(app) else { return };
    let board = app.pcb.view == View::Board;
    let fps = pcb_lib::footprint_names();
    let p = &mut app.ws.pcbs[pi];
    match s {
        Sel::Part(i) if i < p.parts.len() => {
            let kind = pcb_lib::symbol_kind(&p.parts[i].symbol);
            ui.label(RichText::new(kind.map(|k| k.name).unwrap_or("Part")).strong());
            let part = &mut p.parts[i];
            row(ui, "Reference", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.reference).desired_width(120.0));
            });
            row(ui, "Value", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.value).desired_width(120.0));
            });
            row(ui, "Footprint", |ui| {
                egui::ComboBox::from_id_salt("pcb-fp").width(140.0).selected_text(part.footprint.clone()).height(320.0).show_ui(ui, |ui| {
                    let suited: Vec<&str> = kind.map(|k| k.footprints.to_vec()).unwrap_or_default();
                    for f in &suited {
                        ui.selectable_value(&mut part.footprint, f.to_string(), *f);
                    }
                    ui.separator();
                    for f in fps.iter().filter(|f| !suited.contains(&f.as_str())) {
                        ui.selectable_value(&mut part.footprint, f.clone(), f);
                    }
                });
            });
            row(ui, "Part number", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.mpn).desired_width(120.0).hint_text("for the BOM"));
            });
            if matches!(part.symbol.as_str(), "U" | "J") {
                // pin names as one comma separated line, committed when the box loses focus
                if app.pcb.pin_text_for != Some(part.id) {
                    app.pcb.pin_text = part.pin_names.join(", ");
                    app.pcb.pin_text_for = Some(part.id);
                }
                row(ui, "Pin names", |ui| {
                    let r = ui.add(egui::TextEdit::multiline(&mut app.pcb.pin_text).desired_width(140.0).desired_rows(2).hint_text("VCC, GND, … in pin order"));
                    if r.lost_focus() || (r.changed() && !r.has_focus()) {
                        part.pin_names = app.pcb.pin_text.split(',').map(|x| x.trim().to_string()).collect();
                        while part.pin_names.last().is_some_and(|x| x.is_empty()) {
                            part.pin_names.pop();
                        }
                    }
                });
            }
            if board {
                row(ui, "Position", |ui| {
                    ui.add(egui::DragValue::new(&mut part.at[0]).speed(0.05).suffix(" mm").max_decimals(3));
                    ui.add(egui::DragValue::new(&mut part.at[1]).speed(0.05).suffix(" mm").max_decimals(3));
                });
                row(ui, "Rotation", |ui| {
                    ui.add(egui::DragValue::new(&mut part.rot).speed(1.0).range(-360.0..=360.0).suffix("°"));
                });
                row(ui, "Side", |ui| {
                    ui.selectable_value(&mut part.bottom, false, "Top");
                    ui.selectable_value(&mut part.bottom, true, "Bottom");
                });
            }
        }
        Sel::Label(i) if i < p.labels.len() => {
            let l = &mut p.labels[i];
            row(ui, "Net name", |ui| {
                ui.add(egui::TextEdit::singleline(&mut l.text).desired_width(120.0));
            });
            row(ui, "Kind", |ui| {
                ui.selectable_value(&mut l.kind, LabelKind::Net, "Label");
                ui.selectable_value(&mut l.kind, LabelKind::Power, "Power");
                ui.selectable_value(&mut l.kind, LabelKind::Ground, "Ground");
            });
        }
        Sel::Wire(_) => {
            ui.label("Wire · Del removes it");
        }
        Sel::Track(i) if i < p.board.tracks.len() => {
            let t = &mut p.board.tracks[i];
            ui.label(RichText::new(format!("Track · {}", if t.net.is_empty() { "no net" } else { &t.net })).strong());
            row(ui, "Width", |ui| {
                ui.add(egui::DragValue::new(&mut t.width).speed(0.01).range(0.05..=10.0).suffix(" mm").max_decimals(3));
            });
            row(ui, "Layer", |ui| {
                ui.selectable_value(&mut t.layer, Layer::Top, "Top");
                ui.selectable_value(&mut t.layer, Layer::Bottom, "Bottom");
            });
            let net = t.net.clone();
            if ui.button("Width for the whole net").on_hover_text("Give every track of this net this width").clicked() {
                let w = t.width;
                for x in p.board.tracks.iter_mut().filter(|x| x.net == net) {
                    x.width = w;
                }
            }
        }
        Sel::Via(i) if i < p.board.vias.len() => {
            let v = &mut p.board.vias[i];
            ui.label(RichText::new(format!("Via · {}", v.net)).strong());
            row(ui, "Diameter", |ui| {
                ui.add(egui::DragValue::new(&mut v.d).speed(0.01).range(0.2..=5.0).suffix(" mm"));
            });
            row(ui, "Drill", |ui| {
                ui.add(egui::DragValue::new(&mut v.drill).speed(0.01).range(0.1..=4.0).suffix(" mm"));
            });
        }
        Sel::Zone(i) if i < p.board.zones.len() => {
            let nets: Vec<String> = app.pcb.cache.nl.nets.iter().map(|n| n.name.clone()).collect();
            let z = &mut p.board.zones[i];
            ui.label(RichText::new("Copper pour").strong());
            row(ui, "Net", |ui| {
                egui::ComboBox::from_id_salt("pcb-zone-net").selected_text(z.net.clone()).show_ui(ui, |ui| {
                    for n in &nets {
                        ui.selectable_value(&mut z.net, n.clone(), n);
                    }
                });
            });
            row(ui, "Layer", |ui| {
                ui.selectable_value(&mut z.layer, Layer::Top, "Top");
                ui.selectable_value(&mut z.layer, Layer::Bottom, "Bottom");
            });
        }
        Sel::Hole(i) if i < p.board.holes.len() => {
            let h = &mut p.board.holes[i];
            row(ui, "Diameter", |ui| {
                ui.add(egui::DragValue::new(&mut h.d).speed(0.05).range(0.5..=20.0).suffix(" mm"));
            });
            row(ui, "Position", |ui| {
                ui.add(egui::DragValue::new(&mut h.at[0]).speed(0.05).suffix(" mm"));
                ui.add(egui::DragValue::new(&mut h.at[1]).speed(0.05).suffix(" mm"));
            });
        }
        _ => {
            app.pcb.sel = None;
        }
    }
}

fn board_ui(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    let nets: Vec<String> = app.pcb.cache.nl.nets.iter().map(|n| n.name.clone()).collect();
    let b = &mut app.ws.pcbs[pi].board;
    let (lo, hi) = b.bbox();
    let (mut w, mut h) = (hi[0] - lo[0], hi[1] - lo[1]);
    // the corner radius of a rectangular outline (its first point sits where the bottom-right arc starts)
    let r0 = if b.outline.len() > 4 { (hi[0] - b.outline[0][0]).max(0.0) } else { 0.0 };
    let mut r = r0;
    row(ui, "Size", |ui| {
        ui.add(egui::DragValue::new(&mut w).speed(0.5).range(5.0..=1000.0).suffix(" mm").max_decimals(2));
        ui.label("×");
        ui.add(egui::DragValue::new(&mut h).speed(0.5).range(5.0..=1000.0).suffix(" mm").max_decimals(2));
    });
    row(ui, "Corners", |ui| {
        ui.add(egui::DragValue::new(&mut r).speed(0.1).range(0.0..=50.0).suffix(" mm").max_decimals(2)).on_hover_text("Corner radius");
    });
    if (w - (hi[0] - lo[0])).abs() > 1e-9 || (h - (hi[1] - lo[1])).abs() > 1e-9 || (r - r0).abs() > 1e-9 {
        b.outline = pcb::rect_outline(w, h, r).into_iter().map(|q| pcb::add(q, lo)).collect();
        for z in &mut b.zones {
            if z.outline.len() >= 3 {
                z.outline = b.outline.clone();
            }
        }
    }
    row(ui, "Thickness", |ui| {
        egui::ComboBox::from_id_salt("pcb-thk").selected_text(format!("{} mm", b.thickness)).show_ui(ui, |ui| {
            for t in [0.8, 1.0, 1.2, 1.6, 2.0] {
                ui.selectable_value(&mut b.thickness, t, format!("{t} mm"));
            }
        });
    });
    row(ui, "Colour", |ui| {
        egui::ComboBox::from_id_salt("pcb-col").selected_text(if b.color.is_empty() { "green".to_string() } else { b.color.clone() }).show_ui(ui, |ui| {
            for c in ["green", "black", "blue", "red", "white", "purple"] {
                ui.selectable_value(&mut b.color, if c == "green" { String::new() } else { c.to_string() }, c);
            }
        });
    });
    ui.collapsing("Design rules", |ui| {
        let r = &mut b.rules;
        let num = |ui: &mut Ui, label: &str, v: &mut f64, tip: &str| {
            row(ui, label, |ui| {
                ui.add(egui::DragValue::new(v).speed(0.005).range(0.05..=5.0).suffix(" mm").max_decimals(3)).on_hover_text(tip);
            });
        };
        num(ui, "Clearance", &mut r.clearance, "Copper to copper of another net");
        num(ui, "Track", &mut r.track, "Width of new signal tracks");
        num(ui, "Power track", &mut r.power_track, "Width of new tracks on nets named by a power or ground port");
        num(ui, "Via", &mut r.via_d, "Via pad diameter");
        num(ui, "Via drill", &mut r.via_drill, "Via hole diameter");
        num(ui, "Edge", &mut r.edge, "Copper to the board edge");
        num(ui, "Min. track", &mut r.min_track, "The fab's thinnest track");
        num(ui, "Min. drill", &mut r.min_drill, "The fab's smallest hole");
    });
    ui.add_space(4.0);
    // a pour over the whole board on the active layer
    if app.pcb.zone_net.is_empty() || !nets.contains(&app.pcb.zone_net) {
        app.pcb.zone_net = nets.iter().find(|n| n.eq_ignore_ascii_case("GND")).or(nets.first()).cloned().unwrap_or_default();
    }
    let mut go = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("pcb-pour-net").width(90.0).selected_text(app.pcb.zone_net.clone()).show_ui(ui, |ui| {
            for n in &nets {
                ui.selectable_value(&mut app.pcb.zone_net, n.clone(), n);
            }
        });
        go = ui.button(format!("Pour on {}", app.pcb.layer.label().to_lowercase())).on_hover_text("Fill the whole board on the active layer with this net's copper").clicked();
    });
    if go && !app.pcb.zone_net.is_empty() {
        let n = app.pcb.zone_net.clone();
        pour(app, n);
    }
    ui.horizontal(|ui| {
        if ui.button("Place all").on_hover_text("Lay every part out on the board again").clicked() {
            edit(app, |p| {
                crate::pcb_route::auto_place(p, true);
            });
        }
        if ui.button("Remove tracks").on_hover_text("Delete every track and via (Ctrl+Z brings them back)").clicked() {
            edit(app, |p| {
                p.board.tracks.clear();
                p.board.vias.clear();
            });
        }
    });
}

fn layers_ui(app: &mut App, ui: &mut Ui) {
    let s = &mut app.pcb.show;
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut s.top, "Top");
        ui.checkbox(&mut s.bottom, "Bottom");
        ui.checkbox(&mut s.silk, "Silkscreen");
        ui.checkbox(&mut s.zones, "Pours");
        ui.checkbox(&mut s.air, "Ratsnest");
    });
}

fn issues_ui(app: &mut App, ui: &mut Ui, issues: &[Issue]) {
    if issues.is_empty() {
        ui.label(RichText::new("No problems found").color(theme::muted()));
        return;
    }
    for (k, is) in issues.iter().enumerate().take(200) {
        let col = if is.error { theme::error_soft() } else { theme::muted() };
        let r = ui.add(egui::Label::new(RichText::new(format!("{} {}", if is.error { "●" } else { "○" }, is.message)).color(col).size(12.0)).sense(Sense::click()));
        if r.on_hover_text("Show it").clicked() {
            app.pcb.center[1] = is.at;
            app.pcb.zoom[1] = app.pcb.zoom[1].max(12.0);
        }
        let _ = k;
    }
    if issues.len() > 200 {
        ui.label(format!("… and {} more", issues.len() - 200));
    }
}

fn nets_ui(app: &mut App, ui: &mut Ui) {
    let nets: Vec<(String, usize)> = app.pcb.cache.nl.nets.iter().map(|n| (n.name.clone(), n.pins.len())).collect();
    if nets.is_empty() {
        ui.label(RichText::new("No connections yet: wire pins together, or give them the same net label").color(theme::muted()).size(12.0));
        return;
    }
    let unrouted = app.pcb.cache.air.len();
    if unrouted > 0 {
        ui.label(RichText::new(format!("{unrouted} connections to route")).color(theme::muted()).size(12.0));
    } else {
        ui.label(RichText::new("Every connection is routed").color(theme::muted()).size(12.0));
    }
    for (n, k) in nets {
        let on = app.pcb.highlight.as_deref() == Some(n.as_str());
        if ui.add(egui::Button::selectable(on, RichText::new(format!("{n}  ({k})")).size(12.5))).on_hover_text("Highlight this net").clicked() {
            app.pcb.highlight = if on { None } else { Some(n) };
        }
    }
}

pub(crate) fn commands_list() -> Vec<(&'static str, &'static str, fn(&mut App))> {
    vec![
        ("Schematic view", "pcbSchematic", |a| set_view(a, View::Schematic)),
        ("Board view", "pcbBoard", |a| set_view(a, View::Board)),
        ("Wire", "pcbWire", |a| set_tool(a, Tool::Wire)),
        ("Net label", "pcbLabel", |a| set_tool(a, Tool::Label(LabelKind::Net))),
        ("Route tracks", "pcbRoute", |a| {
            set_view(a, View::Board);
            set_tool(a, Tool::Route)
        }),
        ("Autoroute", "pcbAutoroute", autoroute),
        ("Place parts on the board", "pcbPlace", auto_place),
        ("Design rule check", "pcbDrc", run_drc),
        ("Export Gerbers…", "export", export_fab),
        ("3D board (Part Studio)", "cube", to_3d),
    ]
}

// ---------------------------------------------------------------- viewport

struct Xf {
    rect: egui::Rect,
    zoom: f32,
    center: P,
}

impl Xf {
    fn s(&self, p: P) -> Pos2 {
        Pos2::new(self.rect.center().x + ((p[0] - self.center[0]) as f32) * self.zoom, self.rect.center().y - ((p[1] - self.center[1]) as f32) * self.zoom)
    }
    fn w(&self, s: Pos2) -> P {
        [self.center[0] + ((s.x - self.rect.center().x) / self.zoom) as f64, self.center[1] - ((s.y - self.rect.center().y) / self.zoom) as f64]
    }
}

fn c32(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn convex(c: &[P]) -> bool {
    let n = c.len();
    let mut sign = 0.0;
    for i in 0..n {
        let (a, b, d) = (c[i], c[(i + 1) % n], c[(i + 2) % n]);
        let cr = (b[0] - a[0]) * (d[1] - b[1]) - (b[1] - a[1]) * (d[0] - b[0]);
        if cr.abs() > 1e-12 {
            if sign == 0.0 {
                sign = cr.signum();
            } else if cr.signum() != sign {
                return false;
            }
        }
    }
    true
}

fn paint(painter: &egui::Painter, xf: &Xf, list: &[Draw]) {
    let clip = painter.clip_rect().expand(20.0);
    for d in list {
        let col = c32(d.rgba);
        match &d.prim {
            Prim::Seg { a, b, w } => {
                let (p, q) = (xf.s(*a), xf.s(*b));
                if !clip.intersects(egui::Rect::from_two_pos(p, q)) {
                    continue;
                }
                let px = (*w as f32 * xf.zoom).max(1.0);
                painter.line_segment([p, q], Stroke::new(px, col));
                if px > 2.5 {
                    painter.circle_filled(p, px / 2.0, col);
                    painter.circle_filled(q, px / 2.0, col);
                }
            }
            Prim::Circle { c, r, w, fill } => {
                let p = xf.s(*c);
                let rr = (*r as f32 * xf.zoom).max(0.8);
                if *fill {
                    painter.circle_filled(p, rr, col);
                } else {
                    painter.circle_stroke(p, rr, Stroke::new((*w as f32 * xf.zoom).max(1.0), col));
                }
            }
            Prim::Fill(cs) => {
                if cs.is_empty() || cs[0].len() < 3 {
                    continue;
                }
                if cs.len() == 1 && convex(&cs[0]) {
                    painter.add(egui::Shape::convex_polygon(cs[0].iter().map(|p| xf.s(*p)).collect(), col, Stroke::NONE));
                    continue;
                }
                // general polygons with holes: triangulate
                let mut flat: Vec<f64> = vec![];
                let mut holes: Vec<usize> = vec![];
                for (k, c) in cs.iter().enumerate() {
                    if k > 0 {
                        holes.push(flat.len() / 2);
                    }
                    for p in c {
                        flat.extend([p[0], p[1]]);
                    }
                }
                let Ok(idx) = earcutr::earcut(&flat, &holes, 2) else { continue };
                let mut mesh = egui::Mesh::default();
                for k in 0..flat.len() / 2 {
                    mesh.colored_vertex(xf.s([flat[2 * k], flat[2 * k + 1]]), col);
                }
                mesh.indices = idx.into_iter().map(|i| i as u32).collect();
                painter.add(egui::Shape::mesh(mesh));
            }
        }
    }
}

/// The ghost of the part being placed, as a part at the cursor.
fn ghost_part(key: &str, at: P, rot: u8, mirror: bool) -> Option<pcb::Part> {
    let k = pcb_lib::symbol_kind(key)?;
    Some(pcb::Part { reference: format!("{}?", k.prefix), value: k.value.into(), symbol: k.key.into(), footprint: k.footprint.into(), sch: at, sch_rot: rot, sch_mirror: mirror, ..Default::default() })
}

/// Hit-test the schematic at a world point.
fn sch_hit(p: &Pcb, at: P, tol: f64) -> Option<Sel> {
    for (i, l) in p.labels.iter().enumerate() {
        if pcb::dist(l.at, at) < tol * 1.5 || label_box_hit(l, at) {
            return Some(Sel::Label(i));
        }
    }
    for (i, w) in p.wires.iter().enumerate() {
        if pcb::seg_point(w.a, w.b, at) < tol {
            return Some(Sel::Wire(i));
        }
    }
    for (i, part) in p.parts.iter().enumerate().rev() {
        let (lo, hi) = pcb_export::sch_bbox(part);
        if at[0] >= lo[0] - 0.5 && at[0] <= hi[0] + 0.5 && at[1] >= lo[1] - 0.5 && at[1] <= hi[1] + 0.5 {
            return Some(Sel::Part(i));
        }
    }
    None
}

fn label_box_hit(l: &Label, at: P) -> bool {
    let w = pcb_lib::text_width(&l.text, 1.3);
    match l.kind {
        LabelKind::Net if l.rot % 2 == 0 => {
            let x0 = if l.rot % 4 == 2 { l.at[0] - w - 0.5 } else { l.at[0] };
            at[0] >= x0 && at[0] <= x0 + w + 0.5 && at[1] >= l.at[1] && at[1] <= l.at[1] + 1.8
        }
        LabelKind::Ground => (at[0] - l.at[0]).abs() < 1.8 && at[1] <= l.at[1] && at[1] >= l.at[1] - 3.8,
        LabelKind::Power => (at[0] - l.at[0]).abs() < 1.8 && at[1] >= l.at[1] && at[1] <= l.at[1] + 3.2,
        _ => false,
    }
}

/// Hit-test the board at a world point (the active layer first).
fn board_hit(p: &Pcb, at: P, tol: f64, layer: Layer) -> Option<Sel> {
    for (i, v) in p.board.vias.iter().enumerate() {
        if pcb::dist(v.at, at) <= v.d / 2.0 + tol {
            return Some(Sel::Via(i));
        }
    }
    for l in [layer, layer.other()] {
        for (i, t) in p.board.tracks.iter().enumerate().filter(|(_, t)| t.layer == l) {
            if pcb::seg_point(t.a, t.b, at) <= t.width / 2.0 + tol {
                return Some(Sel::Track(i));
            }
        }
    }
    for side in [layer, layer.other()] {
        for (i, part) in p.parts.iter().enumerate().rev().filter(|(_, x)| x.side() == side) {
            let (lo, hi) = p.court_world(part);
            if at[0] >= lo[0] && at[0] <= hi[0] && at[1] >= lo[1] && at[1] <= hi[1] {
                return Some(Sel::Part(i));
            }
        }
    }
    for (i, h) in p.board.holes.iter().enumerate() {
        if pcb::dist(h.at, at) <= h.d / 2.0 + tol {
            return Some(Sel::Hole(i));
        }
    }
    for (i, z) in p.board.zones.iter().enumerate() {
        let n = z.outline.len();
        if (0..n).any(|k| pcb::seg_point(z.outline[k], z.outline[(k + 1) % n], at) < tol * 2.0) {
            return Some(Sel::Zone(i));
        }
    }
    None
}

/// A schematic point snapped to a pin or wire end nearby, else to the grid.
fn sch_snap(p: &Pcb, at: P, tol: f64) -> P {
    let mut best: Option<(f64, P)> = None;
    for sp in p.sch_pins() {
        let d = pcb::dist(sp.at, at);
        if d < tol && best.is_none_or(|b| d < b.0) {
            best = Some((d, sp.at));
        }
    }
    for w in &p.wires {
        for e in [w.a, w.b] {
            let d = pcb::dist(e, at);
            if d < tol && best.is_none_or(|b| d < b.0) {
                best = Some((d, e));
            }
        }
    }
    best.map(|b| b.1).unwrap_or_else(|| pcb::snap(at, GRID))
}

/// The two-segment orthogonal path from a to b.
fn ortho(a: P, b: P, vfirst: bool) -> Vec<P> {
    let corner = if vfirst { [a[0], b[1]] } else { [b[0], a[1]] };
    let mut v = vec![a];
    if pcb::dist(corner, a) > 1e-9 && pcb::dist(corner, b) > 1e-9 {
        v.push(corner);
    }
    v.push(b);
    v
}

/// The two-segment 45° path from a to b: diagonal first, then straight.
fn route45(a: P, b: P) -> Vec<P> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let m = dx.abs().min(dy.abs());
    let corner = [a[0] + m * dx.signum(), a[1] + m * dy.signum()];
    let mut v = vec![a];
    if pcb::dist(corner, a) > 1e-9 && pcb::dist(corner, b) > 1e-9 {
        v.push(corner);
    }
    v.push(b);
    v
}

fn add_wire_path(p: &mut Pcb, pts: &[P]) {
    for s in pts.windows(2) {
        if pcb::dist(s[0], s[1]) > 1e-9 {
            p.wires.push(Wire { a: s[0], b: s[1] });
        }
    }
}

fn close_zone(app: &mut App) {
    let pts = std::mem::take(&mut app.pcb.zone_pts);
    if pts.len() < 3 {
        return;
    }
    let layer = app.pcb.layer;
    let net = app.pcb.zone_net.clone();
    edit(app, |p| p.board.zones.push(Zone { net, layer, outline: pcb::ccw(pts) }));
    set_tool(app, Tool::Select);
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    refresh(app);
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    app.viewport_rect = rect;
    let k = vi(app.pcb.view);
    let board = app.pcb.view == View::Board;
    // fit on first show
    if !app.pcb.fitted[k] || app.pcb.zoom[k] <= 0.0 {
        let p = &app.ws.pcbs[pi];
        let (lo, hi) = if board {
            let mut pts: Vec<P> = p.board.outline.clone();
            for part in &p.parts {
                let (a, b) = p.court_world(part);
                pts.extend([a, b]);
            }
            pcb::bbox(&pts)
        } else if p.parts.is_empty() {
            ([-60.0, -40.0], [60.0, 40.0])
        } else {
            pcb_export::extent(&pcb_export::schematic_scene(p, &app.pcb.cache.nl, None))
        };
        let size = [(hi[0] - lo[0]).max(10.0), (hi[1] - lo[1]).max(10.0)];
        app.pcb.zoom[k] = ((rect.width() - 60.0) / size[0] as f32).min((rect.height() - 60.0) / size[1] as f32).clamp(0.5, 200.0);
        app.pcb.center[k] = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        app.pcb.fitted[k] = true;
    }
    // navigation: wheel zooms at the cursor, middle or right drag pans
    let xf0 = Xf { rect, zoom: app.pcb.zoom[k], center: app.pcb.center[k] };
    if let Some(h) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let pinch = ui.input(|i| i.zoom_delta());
        if scroll.abs() > 0.0 || (pinch - 1.0).abs() > 1e-4 {
            let before = xf0.w(h);
            let z = (app.pcb.zoom[k] * (scroll * 0.0035).exp() * pinch).clamp(0.5, 400.0);
            app.pcb.zoom[k] = z;
            app.pcb.center[k] = [before[0] - ((h.x - rect.center().x) / z) as f64, before[1] + ((h.y - rect.center().y) / z) as f64];
        }
    }
    if resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged_by(egui::PointerButton::Secondary)) {
        let d = resp.drag_delta();
        app.pcb.center[k][0] -= (d.x / app.pcb.zoom[k]) as f64;
        app.pcb.center[k][1] += (d.y / app.pcb.zoom[k]) as f64;
    }
    let xf = Xf { rect, zoom: app.pcb.zoom[k], center: app.pcb.center[k] };
    let tol = (6.0 / xf.zoom) as f64;
    let hover = resp.hover_pos().map(|h| xf.w(h));
    if let Some(h) = hover {
        app.pcb.hover = h;
    }
    let painter = ui.painter_at(rect);
    if board {
        painter.rect_filled(rect, 0.0, if theme::is_dark() { Color32::from_rgb(14, 16, 20) } else { Color32::from_rgb(34, 38, 46) });
        board_grid(&painter, &xf);
    } else {
        painter.rect_filled(rect, 0.0, Color32::from_rgb(250, 249, 244));
        sch_grid(&painter, &xf);
    }
    let clicked = resp.clicked_by(egui::PointerButton::Primary);
    let double = resp.double_clicked();
    let right = resp.clicked_by(egui::PointerButton::Secondary);
    if board {
        board_input(app, pi, &resp, &xf, hover, tol, clicked, double, right);
    } else {
        sch_input(app, pi, &resp, &xf, hover, tol, clicked, double, right);
    }
    refresh(app);
    let p = &app.ws.pcbs[pi];
    let c = &app.pcb.cache;
    if board {
        let list = pcb_export::board_scene(p, &c.nl, &c.pads, &c.fills, &c.air, &BoardView { active: app.pcb.layer, ..app.pcb.show });
        paint(&painter, &xf, &list);
        board_overlay(app, pi, &painter, &xf);
    } else {
        let list = pcb_export::schematic_scene(p, &c.nl, app.pcb.highlight.as_deref());
        paint(&painter, &xf, &list);
        sch_overlay(app, pi, &painter, &xf);
    }
    // the cursor position
    if let Some(h) = hover {
        let g = if board { h } else { pcb::snap(h, GRID) };
        let txt = format!("X {:.2}  Y {:.2} mm", g[0], g[1]);
        let col = if board { Color32::from_gray(200) } else { Color32::from_gray(90) };
        painter.text(rect.right_bottom() + egui::vec2(-12.0, -10.0), egui::Align2::RIGHT_BOTTOM, txt, egui::FontId::proportional(11.5), col);
    }
    if app.pcb.tool != Tool::Select || app.pcb.drag.is_some() {
        ui.ctx().request_repaint();
    }
}

fn sch_grid(painter: &egui::Painter, xf: &Xf) {
    let step = GRID * if xf.zoom < 3.0 { 4.0 } else { 1.0 };
    if (step as f32 * xf.zoom) < 6.0 {
        return;
    }
    let lo = xf.w(xf.rect.left_bottom());
    let hi = xf.w(xf.rect.right_top());
    let col = Color32::from_gray(205);
    let mut x = (lo[0] / step).floor() * step;
    while x <= hi[0] {
        let mut y = (lo[1] / step).floor() * step;
        while y <= hi[1] {
            painter.rect_filled(egui::Rect::from_center_size(xf.s([x, y]), egui::vec2(1.2, 1.2)), 0.0, col);
            y += step;
        }
        x += step;
    }
}

fn board_grid(painter: &egui::Painter, xf: &Xf) {
    let mut step = 1.0;
    while (step as f32 * xf.zoom) < 12.0 {
        step *= 5.0;
    }
    let lo = xf.w(xf.rect.left_bottom());
    let hi = xf.w(xf.rect.right_top());
    let col = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    let mut x = (lo[0] / step).floor() * step;
    while x <= hi[0] {
        let mut y = (lo[1] / step).floor() * step;
        while y <= hi[1] {
            painter.rect_filled(egui::Rect::from_center_size(xf.s([x, y]), egui::vec2(1.5, 1.5)), 0.0, col);
            y += step;
        }
        x += step;
    }
}

#[allow(clippy::too_many_arguments)]
fn sch_input(app: &mut App, pi: usize, resp: &egui::Response, xf: &Xf, hover: Option<P>, tol: f64, clicked: bool, double: bool, right: bool) {
    let tool = app.pcb.tool.clone();
    match tool {
        Tool::Place(key) => {
            if clicked {
                if let Some(h) = hover {
                    let (rot, mir) = (app.pcb.place_rot, app.pcb.place_mirror);
                    let mut err = None;
                    let mut new = None;
                    edit(app, |p| match p.add_part(&key, None, None, h) {
                        Ok(i) => {
                            p.parts[i].sch_rot = rot;
                            p.parts[i].sch_mirror = mir;
                            new = Some(i);
                        }
                        Err(e) => err = Some(e),
                    });
                    if let Some(e) = err {
                        app.flash(e, ToastKind::Error);
                    }
                    app.pcb.sel = new.map(Sel::Part);
                }
            }
            if right {
                set_tool(app, Tool::Select);
            }
        }
        Tool::Wire => {
            let p = &app.ws.pcbs[pi];
            if let (true, Some(h)) = (clicked, hover) {
                let at = sch_snap(p, h, tol * 1.5);
                if app.pcb.wire.is_empty() {
                    app.pcb.wire.push(at);
                } else {
                    let start = *app.pcb.wire.last().unwrap();
                    let path = ortho(start, at, app.pcb.wire_vfirst);
                    // ending on a pin or another wire finishes it
                    let ends_on = p.sch_pins().iter().any(|x| pcb::dist(x.at, at) < 1e-6) || p.wires.iter().any(|w| pcb::seg_point(w.a, w.b, at) < 1e-6);
                    edit(app, |p| add_wire_path(p, &path));
                    if ends_on || double {
                        app.pcb.wire.clear();
                    } else {
                        app.pcb.wire.push(at);
                    }
                }
            }
            if right {
                app.pcb.wire.clear();
            }
        }
        Tool::Label(kind) => {
            if let (true, Some(h)) = (clicked, hover) {
                let p = &app.ws.pcbs[pi];
                let at = sch_snap(p, h, tol * 1.5);
                let text = if app.pcb.label_text.trim().is_empty() { default_label(&app.pcb.tool).to_string() } else { app.pcb.label_text.trim().to_string() };
                let mut idx = 0;
                edit(app, |p| {
                    p.labels.push(Label { at, text, kind, rot: 0 });
                    idx = p.labels.len() - 1;
                });
                if kind == LabelKind::Net {
                    // the next label gets the next number
                    app.pcb.label_text = bump(&app.pcb.label_text);
                }
                app.pcb.sel = Some(Sel::Label(idx));
            }
            if right {
                set_tool(app, Tool::Select);
            }
        }
        _ => {
            let p = &app.ws.pcbs[pi];
            if resp.drag_started_by(egui::PointerButton::Primary) {
                let start = resp.interact_pointer_pos().map(|s| xf.w(s));
                if let Some(at) = start {
                    if let Some(Sel::Part(i)) = sch_hit(p, at, tol) {
                        app.pcb.sel = Some(Sel::Part(i));
                        let pins: Vec<P> = p.sch_pins().into_iter().filter(|x| x.part == i).map(|x| x.at).collect();
                        let mut ends = vec![];
                        for (k, w) in p.wires.iter().enumerate() {
                            if pins.iter().any(|q| pcb::dist(*q, w.a) < 1e-6) {
                                ends.push((k, false, w.a));
                            }
                            if pins.iter().any(|q| pcb::dist(*q, w.b) < 1e-6) {
                                ends.push((k, true, w.b));
                            }
                        }
                        app.pcb.drag = Some(Drag { from: at, orig: p.parts[i].sch, ends });
                        let before = p.clone();
                        app.pcb.last_ck = None;
                        record(app, before);
                    } else if let Some(Sel::Label(i)) = sch_hit(p, at, tol) {
                        app.pcb.sel = Some(Sel::Label(i));
                        app.pcb.drag = Some(Drag { from: at, orig: p.labels[i].at, ends: vec![] });
                        let before = p.clone();
                        app.pcb.last_ck = None;
                        record(app, before);
                    }
                }
            }
            if let (Some(d), Some(h), true) = (&app.pcb.drag, hover, resp.dragged_by(egui::PointerButton::Primary)) {
                let delta = pcb::snap(pcb::sub(h, d.from), GRID);
                let target = pcb::add(d.orig, delta);
                let ends = d.ends.clone();
                let p = &mut app.ws.pcbs[pi];
                match app.pcb.sel {
                    Some(Sel::Part(i)) if i < p.parts.len() => {
                        p.parts[i].sch = target;
                        for (w, b, orig) in ends {
                            if let Some(w) = p.wires.get_mut(w) {
                                let q = pcb::add(orig, delta);
                                if b {
                                    w.b = q;
                                } else {
                                    w.a = q;
                                }
                            }
                        }
                    }
                    Some(Sel::Label(i)) if i < p.labels.len() => p.labels[i].at = target,
                    _ => {}
                }
                app.pcb.dirty = true;
            }
            if resp.drag_stopped() {
                app.pcb.drag = None;
                app.pcb.dirty = true;
            }
            if clicked {
                app.pcb.sel = hover.and_then(|h| sch_hit(&app.ws.pcbs[pi], h, tol));
                if let Some(Sel::Wire(_)) = app.pcb.sel {
                    // show the wire's net
                    let p = &app.ws.pcbs[pi];
                    let h = hover.unwrap();
                    let nl = &app.pcb.cache.nl;
                    let net = p.sch_pins().into_iter().find_map(|x| {
                        let w = p.wires.iter().any(|w| pcb::seg_point(w.a, w.b, x.at) < 1e-3 && pcb::seg_point(w.a, w.b, h) < tol);
                        if w { nl.net_of(x.part, &x.num).map(|n| nl.nets[n].name.clone()) } else { None }
                    });
                    app.pcb.highlight = net;
                }
            }
        }
    }
}

/// "NET1" → "NET2"; text without a number is left alone.
fn bump(s: &str) -> String {
    let t = s.trim();
    let digits: String = t.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    if t.is_empty() {
        return "NET2".into();
    }
    if digits.is_empty() {
        return t.to_string();
    }
    let n: u64 = digits.parse().unwrap_or(0);
    format!("{}{}", &t[..t.len() - digits.len()], n + 1)
}

#[allow(clippy::too_many_arguments)]
fn board_input(app: &mut App, pi: usize, resp: &egui::Response, xf: &Xf, hover: Option<P>, tol: f64, clicked: bool, double: bool, right: bool) {
    let tool = app.pcb.tool.clone();
    match tool {
        Tool::Route => {
            let Some(h) = hover else { return };
            if clicked {
                let p = &app.ws.pcbs[pi];
                let c = &app.pcb.cache;
                let layer = app.pcb.layer;
                match &app.pcb.route {
                    None => {
                        // start on a pad, track or via
                        let hit = pcb::net_at(p, &c.nl, &c.pads, h, layer).or_else(|| pcb::net_at(p, &c.nl, &c.pads, h, layer.other()).map(|x| (x.0, x.1)));
                        if let Some((net, at)) = hit {
                            // a surface mount pad on the other side starts there
                            let pad_layer = c.pads.iter().find(|x| pcb::dist(x.at, at) < 1e-9).and_then(|x| x.layer);
                            let l = pad_layer.unwrap_or(layer);
                            app.pcb.layer = l;
                            let w = p.track_width(&c.nl, &net);
                            app.pcb.highlight = if net.is_empty() { None } else { Some(net.clone()) };
                            app.pcb.route = Some(Route { net, layer: l, pts: vec![at], width: w });
                        } else {
                            app.flash_info("Start on a pad, track or via", Some(Duration::from_secs(2)));
                        }
                    }
                    Some(r) => {
                        let start = *r.pts.last().unwrap();
                        let (net, rl, w) = (r.net.clone(), r.layer, r.width);
                        // ending on copper of the same net finishes the route there
                        let target = pcb::net_at(p, &c.nl, &c.pads, h, rl).filter(|x| x.0 == net && pcb::dist(x.1, start) > 1e-6);
                        let end = target.as_ref().map(|x| x.1).unwrap_or(h);
                        let path = route45(start, end);
                        edit(app, |p| {
                            for s in path.windows(2) {
                                p.board.tracks.push(Track { a: s[0], b: s[1], width: w, layer: rl, net: net.clone() });
                            }
                        });
                        if target.is_some() || double {
                            app.pcb.route = None;
                        } else if let Some(r) = &mut app.pcb.route {
                            r.pts.push(end);
                        }
                    }
                }
            }
            if right {
                if app.pcb.route.is_some() {
                    app.pcb.route = None;
                } else {
                    set_tool(app, Tool::Select);
                }
            }
        }
        Tool::Zone => {
            if let (true, Some(h)) = (clicked, hover) {
                app.pcb.zone_pts.push(pcb::snap(h, 0.1));
                if double {
                    close_zone(app);
                }
            }
            if right {
                close_zone(app);
            }
        }
        Tool::Hole => {
            if let (true, Some(h)) = (clicked, hover) {
                let at = pcb::snap(h, 0.1);
                edit(app, |p| p.board.holes.push(Hole { at, d: 3.2 }));
            }
            if right {
                set_tool(app, Tool::Select);
            }
        }
        _ => {
            let p = &app.ws.pcbs[pi];
            if resp.drag_started_by(egui::PointerButton::Primary) {
                if let Some(at) = resp.interact_pointer_pos().map(|s| xf.w(s)) {
                    match board_hit(p, at, tol, app.pcb.layer) {
                        Some(Sel::Part(i)) => {
                            app.pcb.sel = Some(Sel::Part(i));
                            app.pcb.drag = Some(Drag { from: at, orig: p.parts[i].at, ends: vec![] });
                            let before = p.clone();
                            app.pcb.last_ck = None;
                            record(app, before);
                        }
                        Some(Sel::Hole(i)) => {
                            app.pcb.sel = Some(Sel::Hole(i));
                            app.pcb.drag = Some(Drag { from: at, orig: p.board.holes[i].at, ends: vec![] });
                            let before = p.clone();
                            app.pcb.last_ck = None;
                            record(app, before);
                        }
                        _ => {}
                    }
                }
            }
            if let (Some(d), Some(h), true) = (&app.pcb.drag, hover, resp.dragged_by(egui::PointerButton::Primary)) {
                let target = pcb::snap(pcb::add(d.orig, pcb::sub(h, d.from)), 0.05);
                let p = &mut app.ws.pcbs[pi];
                match app.pcb.sel {
                    Some(Sel::Part(i)) if i < p.parts.len() => p.parts[i].at = target,
                    Some(Sel::Hole(i)) if i < p.board.holes.len() => p.board.holes[i].at = target,
                    _ => {}
                }
                app.pcb.dirty = true;
            }
            if resp.drag_stopped() {
                app.pcb.drag = None;
                app.pcb.dirty = true;
            }
            if clicked {
                let p = &app.ws.pcbs[pi];
                app.pcb.sel = hover.and_then(|h| board_hit(p, h, tol, app.pcb.layer));
                // clicking copper shows its net
                let c = &app.pcb.cache;
                app.pcb.highlight = hover.and_then(|h| pcb::net_at(p, &c.nl, &c.pads, h, app.pcb.layer)).map(|x| x.0).filter(|n| !n.is_empty());
            }
        }
    }
}

fn sch_overlay(app: &App, pi: usize, painter: &egui::Painter, xf: &Xf) {
    let p = &app.ws.pcbs[pi];
    let hl = Color32::from_rgb(60, 130, 230);
    match app.pcb.sel {
        Some(Sel::Part(i)) if i < p.parts.len() => {
            let (lo, hi) = pcb_export::sch_bbox(&p.parts[i]);
            painter.rect_stroke(egui::Rect::from_two_pos(xf.s(lo), xf.s(hi)).expand(4.0), 3.0, Stroke::new(1.5, hl), egui::StrokeKind::Outside);
        }
        Some(Sel::Wire(i)) if i < p.wires.len() => {
            painter.line_segment([xf.s(p.wires[i].a), xf.s(p.wires[i].b)], Stroke::new(4.0, hl.gamma_multiply(0.6)));
        }
        Some(Sel::Label(i)) if i < p.labels.len() => {
            painter.circle_stroke(xf.s(p.labels[i].at), 6.0, Stroke::new(1.5, hl));
        }
        _ => {}
    }
    let h = app.pcb.hover;
    match &app.pcb.tool {
        Tool::Place(key) => {
            if let Some(g) = ghost_part(key, pcb::snap(h, GRID), app.pcb.place_rot, app.pcb.place_mirror) {
                let ghost = Pcb { parts: vec![g], ..Default::default() };
                let mut list = pcb_export::schematic_scene(&ghost, &Netlist::default(), None);
                for d in &mut list {
                    d.rgba[3] = 120;
                }
                paint(painter, xf, &list);
            }
        }
        Tool::Wire => {
            let at = sch_snap(p, h, (9.0 / xf.zoom) as f64);
            painter.circle_stroke(xf.s(at), 4.0, Stroke::new(1.2, Color32::from_rgb(0, 128, 0)));
            if let Some(start) = app.pcb.wire.last() {
                let path = ortho(*start, at, app.pcb.wire_vfirst);
                for s in path.windows(2) {
                    painter.line_segment([xf.s(s[0]), xf.s(s[1])], Stroke::new(1.5, Color32::from_rgb(0, 160, 0)));
                }
            }
        }
        Tool::Label(kind) => {
            let at = sch_snap(p, h, (9.0 / xf.zoom) as f64);
            let text = if app.pcb.label_text.trim().is_empty() { default_label(&app.pcb.tool).to_string() } else { app.pcb.label_text.trim().to_string() };
            let mut list = vec![];
            pcb_export::label(&mut list, &Label { at, text, kind: *kind, rot: 0 });
            for d in &mut list {
                d.rgba[3] = 140;
            }
            paint(painter, xf, &list);
        }
        _ => {}
    }
}

fn board_overlay(app: &App, pi: usize, painter: &egui::Painter, xf: &Xf) {
    let p = &app.ws.pcbs[pi];
    let hl = Color32::from_rgb(120, 200, 255);
    // the highlighted net's pads
    if let Some(n) = &app.pcb.highlight {
        if let Some(ni) = app.pcb.cache.nl.by_name(n) {
            for pad in app.pcb.cache.pads.iter().filter(|x| x.net == Some(ni)) {
                let c = pcb_export::pad_outline(pad, 0.1);
                let pts: Vec<Pos2> = c.iter().map(|q| xf.s(*q)).collect();
                painter.add(egui::Shape::closed_line(pts, Stroke::new(1.5, Color32::WHITE)));
            }
        }
    }
    match app.pcb.sel {
        Some(Sel::Part(i)) if i < p.parts.len() => {
            let (lo, hi) = p.court_world(&p.parts[i]);
            painter.rect_stroke(egui::Rect::from_two_pos(xf.s(lo), xf.s(hi)), 2.0, Stroke::new(1.5, hl), egui::StrokeKind::Outside);
            painter.text(xf.s([lo[0], hi[1]]) + egui::vec2(0.0, -4.0), egui::Align2::LEFT_BOTTOM, format!("{} {}", p.parts[i].reference, p.parts[i].value), egui::FontId::proportional(12.0), hl);
        }
        Some(Sel::Track(i)) if i < p.board.tracks.len() => {
            let t = &p.board.tracks[i];
            painter.line_segment([xf.s(t.a), xf.s(t.b)], Stroke::new((t.width as f32 * xf.zoom).max(2.0) + 3.0, hl.gamma_multiply(0.5)));
        }
        Some(Sel::Via(i)) if i < p.board.vias.len() => {
            painter.circle_stroke(xf.s(p.board.vias[i].at), (p.board.vias[i].d as f32 * xf.zoom) / 2.0 + 3.0, Stroke::new(1.5, hl));
        }
        Some(Sel::Hole(i)) if i < p.board.holes.len() => {
            painter.circle_stroke(xf.s(p.board.holes[i].at), (p.board.holes[i].d as f32 * xf.zoom) / 2.0 + 3.0, Stroke::new(1.5, hl));
        }
        Some(Sel::Zone(i)) if i < p.board.zones.len() => {
            let pts: Vec<Pos2> = p.board.zones[i].outline.iter().map(|q| xf.s(*q)).collect();
            painter.add(egui::Shape::closed_line(pts, Stroke::new(2.0, hl)));
        }
        _ => {}
    }
    if let Some(issues) = &app.pcb.issues {
        for is in issues {
            let c = xf.s(is.at);
            let col = if is.error { Color32::from_rgb(255, 80, 80) } else { Color32::from_rgb(255, 200, 60) };
            painter.circle_stroke(c, 7.0, Stroke::new(2.0, col));
        }
    }
    let h = app.pcb.hover;
    if let Some(r) = &app.pcb.route {
        let col = if r.layer == Layer::Top { pcb_export::colors::TOP } else { pcb_export::colors::BOTTOM };
        let col = Color32::from_rgba_unmultiplied(col[0], col[1], col[2], 170);
        let path = route45(*r.pts.last().unwrap(), h);
        let wpx = (r.width as f32 * xf.zoom).max(1.5);
        for s in path.windows(2) {
            painter.line_segment([xf.s(s[0]), xf.s(s[1])], Stroke::new(wpx, col));
        }
        painter.text(xf.s(h) + egui::vec2(12.0, -12.0), egui::Align2::LEFT_BOTTOM, format!("{} · {} · {} mm", r.net, r.layer.label(), r.width), egui::FontId::proportional(11.5), Color32::WHITE);
    }
    if app.pcb.tool == Tool::Zone && !app.pcb.zone_pts.is_empty() {
        let mut pts: Vec<Pos2> = app.pcb.zone_pts.iter().map(|q| xf.s(*q)).collect();
        pts.push(xf.s(pcb::snap(h, 0.1)));
        painter.add(egui::Shape::closed_line(pts, Stroke::new(1.5, hl)));
    }
    if app.pcb.tool == Tool::Hole {
        painter.circle_stroke(xf.s(pcb::snap(h, 0.1)), 1.6 * xf.zoom, Stroke::new(1.2, hl));
    }
}
