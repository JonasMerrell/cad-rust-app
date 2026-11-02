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
    /// The schematic with probes and the waveform plot.
    Simulate,
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
    /// Drawing a keepout area (like a pour).
    Keepout,
    /// Marking pins as left open on purpose.
    NoConnect,
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
    Keepout(usize),
}

struct Drag {
    /// Where the drag started, in world mm.
    from: P,
    /// The part's position then.
    orig: P,
    /// Wire ends attached to the part's pins: (wire, end b?, original point).
    ends: Vec<(usize, bool, P)>,
    /// The PCB before the drag, for its undo step.
    before: Pcb,
}

struct Route {
    net: String,
    layer: Layer,
    pts: Vec<P>,
    width: f64,
}

/// Derived data, rebuilt when the PCB changes.
#[derive(Default)]
pub(crate) struct Cache {
    pcb: Option<usize>,
    pub(crate) nl: Netlist,
    pads: Vec<WPad>,
    fills: Vec<pcb::Fill>,
    air: Vec<pcb::Airwire>,
    /// Each net's pins, to follow renamed nets on the next rebuild.
    pins: pcb::NetPins,
}

pub struct PcbUi {
    pub view: View,
    pub tool: Tool,
    /// Per view: millimetres per point... pixels per mm and the centre.
    zoom: [f32; 2],
    center: [P; 2],
    fitted: [bool; 2],
    /// The viewport size of an automatic fit not yet changed by the user: the fit is redone when
    /// the viewport settles to another size (the first frames lay the panels out).
    fit_size: [Option<egui::Vec2>; 2],
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
    /// Counts the undo steps recorded (the sidebar records an edit unless one was made for it).
    steps: u64,
    dirty: bool,
    pub(crate) cache: Cache,
    pub issues: Option<Vec<Issue>>,
    /// The last electrical rule check, shown on the schematic.
    pub erc: Option<Vec<Issue>>,
    /// Interactive routing flows round other copper (walkaround).
    walkaround: bool,
    /// The walkaround grid for the net being routed (net, width, board edit count) and the last path.
    walk: Option<(crate::pcb_route::Walk, String, f64, u64)>,
    walk_path: Option<(P, P, Layer, Option<Vec<P>>)>,
    /// Counts board changes, so the walkaround grid is rebuilt after one.
    cache_gen: u64,
    /// The issue last picked in a check list (view index, issue index), drawn bolder.
    issue_shown: Option<(usize, usize)>,
    lib_search: String,
    /// The IPC-7351 footprint wizard's inputs while it is open.
    wizard: Option<crate::pcb_ipc::IpcSpec>,
    label_text: String,
    zone_net: String,
    highlight: Option<String>,
    pub(crate) hover: P,
    pin_text: String,
    pin_text_for: Option<u64>,
    pub sim: crate::circuit_ui::SimUi,
    pub dk: crate::digikey_ui::DkUi,
    /// The new field being added to the selected part (name, value).
    field_new: (String, String),
    /// The PCB tab the history and selection belong to.
    tab: Option<usize>,
}

impl Default for PcbUi {
    fn default() -> Self {
        PcbUi {
            view: View::Schematic,
            tool: Tool::Select,
            zoom: [0.0; 2],
            center: [[0.0; 2]; 2],
            fitted: [false; 2],
            fit_size: [None; 2],
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
            steps: 0,
            dirty: true,
            cache: Cache::default(),
            issues: None,
            erc: None,
            walkaround: true,
            walk: None,
            walk_path: None,
            cache_gen: 0,
            issue_shown: None,
            lib_search: String::new(),
            wizard: None,
            label_text: String::new(),
            zone_net: String::new(),
            highlight: None,
            hover: [0.0; 2],
            pin_text: String::new(),
            pin_text_for: None,
            sim: Default::default(),
            dk: Default::default(),
            field_new: Default::default(),
            tab: None,
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

impl PcbUi {
    /// Going to a PCB tab: the undo history and selection of another tab don't carry over.
    pub fn enter_tab(&mut self, i: usize) {
        if self.tab.is_some_and(|t| t != i) {
            self.undo.clear();
            self.redo.clear();
            self.last_ck = None;
            self.forget_selection();
            self.fitted = [false; 2];
        }
        self.tab = Some(i);
    }

    fn forget_selection(&mut self) {
        self.dirty = true;
        self.pin_text_for = None;
        self.sel = None;
        self.drag = None;
        self.route = None;
        self.wire.clear();
        self.zone_pts.clear();
        self.issues = None;
        self.erc = None;
        self.highlight = None;
    }
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
    // the next edit is a step of its own, and text boxes show the restored values
    app.pcb.last_ck = None;
    app.pcb.pin_text_for = None;
    app.pcb.zone_pts.clear();
    app.pcb.sel = None;
    app.pcb.drag = None;
    app.pcb.route = None;
    app.pcb.wire.clear();
    app.pcb.zone_pts.clear();
    app.pcb.issues = None;
    app.pcb.erc = None;
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
        app.pcb.steps += 1;
    }
    app.pcb.last_ck = Some(now);
    app.pcb.dirty = true;
    app.pcb.issues = None;
    app.pcb.erc = None;
}

/// Make an edit as its own undo step.
pub(crate) fn edit(app: &mut App, f: impl FnOnce(&mut Pcb)) {
    let Some(i) = index(app) else { return };
    let before = app.ws.pcbs[i].clone();
    f(&mut app.ws.pcbs[i]);
    if app.ws.pcbs[i] != before {
        app.pcb.last_ck = None;
        record(app, before);
        app.pcb.last_ck = None;
    }
}

/// A drag of a part, label or hole ended: one undo step if it moved anything.
fn finish_drag(app: &mut App) {
    let Some(d) = app.pcb.drag.take() else { return };
    app.pcb.dirty = true;
    let Some(i) = index(app) else { return };
    if app.ws.pcbs[i] != d.before {
        app.pcb.last_ck = None;
        record(app, d.before);
        app.pcb.last_ck = None;
    }
}

/// The PCB the editor's undo history belongs to (taken before the workspace is replaced).
pub(crate) fn history_pcb(app: &App) -> Option<Pcb> {
    app.pcb.tab.and_then(|t| app.ws.pcbs.get(t).cloned())
}

/// The workspace was replaced (Claude's edit, undoing a tab operation): the editor keeps its
/// history, view and zoom while the PCB `before` (from `history_pcb`) is still there, following it
/// to its new index; with `record_change` a change to it becomes one undo step.
pub(crate) fn ws_replaced(app: &mut App, before: Option<Pcb>, record_change: bool) {
    app.pcb.forget_selection();
    app.pcb.cache.pcb = None;
    let Some(before) = before else { return };
    let Some(j) = app.ws.pcbs.iter().position(|p| !before.uid.is_empty() && p.uid == before.uid) else {
        app.pcb.undo.clear();
        app.pcb.redo.clear();
        app.pcb.last_ck = None;
        app.pcb.tab = None;
        app.pcb.fitted = [false; 2];
        return;
    };
    app.pcb.tab = Some(j);
    if record_change && app.ws.pcbs[j] != before {
        app.pcb.undo.push(before);
        if app.pcb.undo.len() > 200 {
            app.pcb.undo.remove(0);
        }
        app.pcb.redo.clear();
        app.pcb.steps += 1;
        app.pcb.last_ck = None;
    }
}

fn refresh(app: &mut App) {
    let Some(i) = index(app) else { return };
    if !app.pcb.dirty && app.pcb.cache.pcb == Some(i) {
        return;
    }
    // copper follows its net when a schematic edit renames it
    let nl = app.ws.pcbs[i].netlist();
    let prev = (app.pcb.cache.pcb == Some(i)).then(|| std::mem::take(&mut app.pcb.cache.pins));
    app.ws.pcbs[i].retag_copper_from(&nl, prev.as_ref());
    let p = &app.ws.pcbs[i];
    let pins = p.net_pins(&nl);
    let pads = p.pads(&nl);
    // pours are slow to refill: keep the old fill while a part is being dragged
    let fills = if app.pcb.drag.is_some() && app.pcb.cache.pcb == Some(i) && app.pcb.cache.fills.len() == p.board.zones.len() {
        std::mem::take(&mut app.pcb.cache.fills)
    } else {
        pcb::fill_zones(p, &nl, &pads)
    };
    let its = pcb::items(p, &nl, &pads, &fills);
    let air = pcb::ratsnest(&its, &pads, &fills);
    app.pcb.cache = Cache { pcb: Some(i), nl, pads, fills, air, pins };
    app.pcb.dirty = false;
    app.pcb.cache_gen += 1;
}

fn vi(v: View) -> usize {
    match v {
        View::Schematic | View::Simulate => 0,
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
    // R, F and Del act on the selection: drawing tools don't keep one
    if t != Tool::Select {
        app.pcb.sel = None;
    }
    if app.pcb.route.is_some() {
        app.pcb.highlight = None;
    }
    app.pcb.tool = t;
    app.pcb.dk.placing = None;
    app.pcb.wire.clear();
    app.pcb.route = None;
    app.pcb.zone_pts.clear();
    finish_drag(app);
}

/// Start placing library symbol `key` on the schematic.
pub(crate) fn start_place(app: &mut App, key: &str) {
    set_tool(app, Tool::Place(key.to_string()));
    app.pcb.place_rot = 0;
    app.pcb.place_mirror = false;
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
    place_parts(app, false);
}

/// Place the parts that are off the board (`all`: every part again) and say what didn't fit.
fn place_parts(app: &mut App, all: bool) {
    let mut out = None;
    edit(app, |p| out = Some(crate::pcb_route::auto_place(p, all)));
    let Some(out) = out else { return };
    if !out.left.is_empty() {
        app.flash(format!("Placed {}; no room on the board for {}. Make the board bigger or move parts closer", out.placed, out.left.join(", ")), ToastKind::Error);
    } else if out.placed == 0 {
        app.flash_info("Every part is already on the board (drag them, or use Place all to start over)", Some(Duration::from_secs(3)));
    } else {
        app.flash_info(format!("Placed {} parts on the board", out.placed), Some(Duration::from_secs(3)));
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
    app.pcb.issue_shown = None;
    if app.pcb.view != View::Board {
        set_view(app, View::Board);
    }
}

pub(crate) fn run_erc(app: &mut App) {
    refresh(app);
    let Some(p) = pcb_ref(app) else { return };
    let issues = crate::pcb_erc::erc(p, &app.pcb.cache.nl);
    let errors = issues.iter().filter(|i| i.error).count();
    if issues.is_empty() {
        app.flash_info("Electrical rule check passed: no problems found", Some(Duration::from_secs(3)));
    } else {
        app.flash_info(format!("Electrical rule check: {errors} errors, {} warnings (listed in the sidebar)", issues.len() - errors), Some(Duration::from_secs(4)));
    }
    app.pcb.erc = Some(issues);
    app.pcb.issue_shown = None;
    if app.pcb.view != View::Schematic {
        set_view(app, View::Schematic);
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

/// Run the PCB's output job into a ZIP: Gerbers with the X2 job file and drill drawing, BOM,
/// pick and place, IPC-D-356A, ODB++ and the PDF prints, as the job has them turned on.
pub(crate) fn run_output_job(app: &mut App) {
    let Some(p) = pcb_ref(app).cloned() else { return };
    let name = format!("{}-outputs.zip", p.name.replace(['/', '\\'], "_"));
    let Some(path) = app.save_file_dialog("Output job ZIP", &["zip"], name) else { return };
    match crate::pcb_fab::run(&p, &p.outputs, &path) {
        Ok(files) => app.flash_info(format!("Saved {}", files.join(", ")), Some(Duration::from_secs(5))),
        Err(e) => app.flash(format!("Could not run the output job: {e}"), ToastKind::Error),
    }
}

fn output_job_ui(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    let job = &mut app.ws.pcbs[pi].outputs;
    for (k, d) in crate::pcb_fab::OutJob::NAMES {
        if let Some(v) = job.get_mut(k) {
            ui.checkbox(v, d);
        }
    }
    if ui.button("Run output job…").on_hover_text("Write every output that is ticked into one ZIP").clicked() {
        run_output_job(app);
    }
}

pub(crate) fn to_3d(app: &mut App) {
    let Some(i) = index(app) else { return };
    app.sync_ws();
    let n = app.ws.studios.len();
    // the studio as it was, for its undo history
    let old = pcb_export::studio_of(&app.ws, i).map(|s| (s, app.ws.studios[s].doc.clone()));
    match pcb_export::to_studio(&mut app.ws, i) {
        Ok(si) => {
            if si == app.cur_studio {
                app.checkpoint();
                app.doc = app.ws.studios[si].doc.clone();
                app.changed();
            } else {
                app.studio_results.remove(&si);
                // one undo step in that studio's parked history
                if let Some((_, doc)) = old.filter(|(s, _)| *s == si) {
                    let (u, r) = app.studio_undo.entry(si).or_default();
                    u.push(doc);
                    if u.len() > 200 {
                        u.remove(0);
                    }
                    r.clear();
                }
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
        p.board.zones.push(Zone { net, layer, outline, ..Default::default() });
    });
    warn_empty_pour(app);
}

/// After adding a pour (the last zone): say so when it gets no copper.
fn warn_empty_pour(app: &mut App) {
    refresh(app);
    let Some(p) = pcb_ref(app) else { return };
    let Some(z) = p.board.zones.last() else { return };
    if app.pcb.cache.fills.last().is_some_and(|f| f.is_empty()) {
        let msg = format!("The {} pour on {} is empty: it touches no pad, track or via of {} there", z.net, z.layer.label().to_lowercase(), z.net);
        app.flash_info(msg, Some(Duration::from_secs(4)));
    }
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
        Sel::Keepout(i) if i < p.board.keepouts.len() => {
            p.board.keepouts.remove(i);
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
            let Some(part) = p.parts.get_mut(i) else { return };
            if view == View::Schematic {
                part.sch_rot = (part.sch_rot + 1) % 4;
            } else {
                part.rot = (part.rot + 90.0).rem_euclid(360.0);
            }
        }),
        Some(Sel::Label(i)) => edit(app, |p| {
            if let Some(l) = p.labels.get_mut(i) {
                l.rot = (l.rot + 1) % 4;
            }
        }),
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
            let Some(part) = p.parts.get_mut(i) else { return };
            if view == View::Schematic {
                part.sch_mirror = !part.sch_mirror;
            } else {
                part.bottom = !part.bottom;
            }
        });
    }
}

pub fn hint(app: &App) -> String {
    match (&app.pcb.view, &app.pcb.tool) {
        (_, Tool::Place(_)) if crate::digikey_ui::placing_name(app).is_some() => format!("Click to place {} · R rotates, X mirrors · Esc when done", crate::digikey_ui::placing_name(app).unwrap_or_default()),
        (_, Tool::Place(k)) => format!("Click to place {} · R rotates, X mirrors · Esc when done", pcb_lib::kind_info(k).map(|s| s.name).unwrap_or("the part".into())),
        (_, Tool::Wire) if app.pcb.wire.is_empty() => "Click a pin or wire end to start a wire · Esc stops".into(),
        (_, Tool::Wire) => "Click to add a corner, click a pin or wire to finish · / flips the corner · Esc or double-click ends".into(),
        (_, Tool::Label(_)) => "Click a pin or wire to name its net · the text is in the sidebar".into(),
        (_, Tool::Route) if app.pcb.route.is_none() => "Click a pad or track to start routing its net".into(),
        (_, Tool::Route) => "Click to add a corner, click the target pad to finish · V adds a via and changes layer · Esc ends".into(),
        (_, Tool::Zone) => "Click the corners of the pour, double-click or Enter to close it · or use Pour in the sidebar".into(),
        (_, Tool::Hole) => "Click to add a mounting hole".into(),
        (_, Tool::Keepout) => "Click the corners of the keepout, double-click or Enter to close it · tracks, vias and pours stay out".into(),
        (_, Tool::NoConnect) => "Click a pin to mark it as left open on purpose (click again to clear) · Esc when done".into(),
        (View::Schematic, _) => "Add parts from the library, wire their pins (W), name nets (L) · B switches to the board".into(),
        (View::Simulate, _) => crate::circuit_ui::hint(app),
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
    if app.pcb.view == View::Simulate {
        if pressed(egui::Key::F) {
            fit(app);
        }
        if pressed(egui::Key::B) {
            set_view(app, View::Board);
        }
        return crate::circuit_ui::shortcuts(app, ctx);
    }
    if pressed(egui::Key::Escape) {
        if app.pcb.route.is_some() || !app.pcb.wire.is_empty() || !app.pcb.zone_pts.is_empty() {
            if app.pcb.route.is_some() {
                app.pcb.highlight = None;
            }
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
    if pressed(egui::Key::Enter) && matches!(app.pcb.tool, Tool::Zone | Tool::Keepout) {
        close_zone(app);
    }
    // the selection keys only act in the select tool (R and X also turn the part being placed)
    let selecting = app.pcb.tool == Tool::Select;
    let placing = matches!(app.pcb.tool, Tool::Place(_));
    if selecting && (pressed(egui::Key::Delete) || pressed(egui::Key::Backspace)) {
        delete_sel(app);
    }
    if (selecting || placing) && pressed(egui::Key::R) {
        rotate_sel(app);
    }
    if (selecting || placing) && pressed(egui::Key::X) && !board {
        mirror_sel(app);
    }
    if pressed(egui::Key::F) {
        if board && selecting && matches!(app.pcb.sel, Some(Sel::Part(_))) {
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
        if pressed(egui::Key::Q) {
            set_tool(app, Tool::NoConnect);
        }
    } else {
        if pressed(egui::Key::X) {
            set_tool(app, Tool::Route);
        }
        if pressed(egui::Key::L) {
            switch_layer(app, app.pcb.layer.other());
        }
        if pressed(egui::Key::V) {
            via_here(app);
        }
    }
}

/// The active layer; while routing, changing it goes through a via.
fn switch_layer(app: &mut App, l: Layer) {
    if app.pcb.route.as_ref().is_some_and(|r| r.layer != l) {
        via_here(app);
    } else {
        app.pcb.layer = l;
    }
}

/// While routing: a via at the last point, and carry on on the other layer. On a plated hole or
/// an existing via the route just changes layer; on a surface mount pad there is no via.
fn via_here(app: &mut App) {
    let Some(r) = &app.pcb.route else { return };
    let at = *r.pts.last().unwrap();
    let net = r.net.clone();
    let layer = r.layer.other();
    refresh(app);
    let c = &app.pcb.cache;
    let pad = c.pads.iter().find(|x| x.cu().contains(at));
    if pad.is_some_and(|x| x.layer.is_some()) {
        app.flash_info("No via in a surface mount pad: add a corner off the pad first", Some(Duration::from_secs(3)));
        return;
    }
    let through = pad.is_some();
    edit(app, |p| {
        let nr = p.net_rules(&p.netlist(), &net);
        let (d, drill) = (nr.via_d, nr.via_drill);
        if !through && !p.board.vias.iter().any(|v| pcb::dist(v.at, at) < 1e-6) {
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
        if app.tool_button(ui, "pcbSchematic", "Schematic", "The circuit: parts, wires and net labels (B)", true, app.pcb.view == View::Schematic, true).clicked() {
            set_view(app, View::Schematic);
        }
        if app.tool_button(ui, "pcbBoard", "Board", "The layout: footprints, tracks and copper (B)", true, board, true).clicked() {
            set_view(app, View::Board);
        }
        if app.tool_button(ui, "pcbSim", "Simulate", "Simulate the circuit: DC, transient and AC, with probes and plots", true, app.pcb.view == View::Simulate, true).clicked() {
            set_view(app, View::Simulate);
        }
    });
    let t = app.pcb.tool.clone();
    if app.pcb.view == View::Simulate {
        crate::circuit_ui::toolbar(app, ui);
    } else if !board {
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
            if app.tool_button(ui, "pcbNoConnect", "No connect", "Mark pins left open on purpose, so the electrical check doesn't flag them (Q)", false, t == Tool::NoConnect, true).clicked() {
                set_tool(app, Tool::NoConnect);
            }
        });
        App::group(ui, |ui| {
            if app.tool_button(ui, "pcbErc", "Check", "Electrical rule check: open pins, outputs driving each other, unpowered pins, dangling wires", true, app.pcb.erc.is_some(), true).clicked() {
                run_erc(app);
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
            if app.tool_button(ui, "pcbKeepout", "Keepout", "Draw an area that tracks, vias and pours stay out of", false, t == Tool::Keepout, true).clicked() {
                set_tool(app, Tool::Keepout);
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
                    switch_layer(app, l);
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
            if app.tool_button(ui, "pcbDrc", "Check", "Design rule check: clearances by net class, shorts, holes, keepouts, 3D bodies, unrouted nets", true, app.pcb.issues.is_some(), true).clicked() {
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
    let steps = app.pcb.steps;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if app.pcb.view == View::Simulate {
            crate::circuit_ui::sidebar(app, ui);
            return;
        }
        if app.pcb.view == View::Schematic {
            if let Some(issues) = app.pcb.erc.clone() {
                section(ui, "Check", |ui| issues_ui(app, ui, &issues));
            }
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
        if app.pcb.view != View::Board || matches!(app.pcb.sel, Some(Sel::Part(_))) {
            section(ui, "Digi-Key", |ui| crate::digikey_ui::section_ui(app, ui));
        }
        if app.pcb.view == View::Board {
            section(ui, "Board", |ui| board_ui(app, ui));
            section(ui, "Layers", |ui| layers_ui(app, ui));
            section(ui, "Lengths and pairs", |ui| lengths_ui(app, ui));
            section(ui, "Output job", |ui| output_job_ui(app, ui));
            if let Some(issues) = app.pcb.issues.clone() {
                section(ui, "Check", |ui| issues_ui(app, ui, &issues));
            }
        }
        section(ui, "Nets", |ui| nets_ui(app, ui));
    });
    // edits made through edit() have their own undo step
    if app.ws.pcbs[i] != before && app.pcb.drag.is_none() {
        if app.pcb.steps == steps {
            record(app, before);
        } else {
            app.pcb.dirty = true;
        }
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
            start_place(app, s.key);
        }
    }
    let Some(pi) = index(app) else { return };
    let imported: Vec<(String, String, String)> = app.ws.pcbs[pi].symbols.iter().filter(|s| q.is_empty() || s.key.to_lowercase().contains(&q) || s.description.to_lowercase().contains(&q)).map(|s| (s.key.clone(), s.footprint.clone(), s.description.clone())).collect();
    if !imported.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("Imported").color(theme::muted()).size(11.5));
    }
    for (key, fp, desc) in imported {
        let on = app.pcb.tool == Tool::Place(key.clone());
        let name = key.rsplit(':').next().unwrap_or(&key).to_string();
        let r = ui.add_sized([ui.available_width(), 22.0], egui::Button::selectable(on, RichText::new(format!("{name}  ·  {fp}")).size(12.5)));
        if r.on_hover_text(if desc.is_empty() { key.clone() } else { format!("{key}: {desc}") }).clicked() {
            start_place(app, &key);
        }
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Import KiCad…").on_hover_text("Footprints (.kicad_mod), symbols (.kicad_sym) or a ZIP from SnapEDA, Ultra Librarian and the like").clicked() {
            if let Some(path) = app.open_file_dialog("KiCad library", &["kicad_mod", "kicad_sym", "zip", "KICAD_MOD", "KICAD_SYM", "ZIP"]) {
                import_kicad(app, pi, &path);
            }
        }
        if ui.button("Folder…").on_hover_text("Every footprint and symbol in a folder (.pretty, .kicad_symdir)").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                import_kicad(app, pi, &path);
            }
        }
        let open = app.pcb.wizard.is_some();
        if ui.selectable_label(open, "IPC wizard").on_hover_text("Make an IPC-7351 footprint from datasheet dimensions").clicked() {
            app.pcb.wizard = if open { None } else { Some(crate::pcb_ipc::IpcSpec { size: "0805".into(), ..Default::default() }) };
        }
    });
    if app.pcb.wizard.is_some() {
        wizard_ui(app, ui, pi);
    }
}

fn import_kicad(app: &mut App, pi: usize, path: &std::path::Path) {
    match crate::pcb_kicad::import_path(path) {
        Ok(im) => {
            let (nf, ns) = (im.footprints.len(), im.symbols.len());
            let notes = crate::pcb_kicad::add_to(&mut app.ws.pcbs[pi], im);
            let mut msg = format!("Imported {nf} footprint(s) and {ns} symbol(s)");
            if !notes.is_empty() {
                msg += &format!(" · {}", notes.join(" · "));
            }
            app.flash_info(msg, Some(Duration::from_secs(8)));
        }
        Err(e) => app.flash(format!("Could not import: {e}"), ToastKind::Error),
    }
}

fn wizard_ui(app: &mut App, ui: &mut Ui, pi: usize) {
    use crate::pcb_ipc::Family;
    let sel_part = match app.pcb.sel {
        Some(Sel::Part(i)) if i < app.ws.pcbs[pi].parts.len() => Some(i),
        _ => None,
    };
    let Some(w) = app.pcb.wizard.as_mut() else { return };
    let num = |ui: &mut Ui, label: &str, v: &mut f64| {
        row(ui, label, |ui| {
            ui.add(egui::DragValue::new(v).speed(0.01).range(0.0..=100.0).max_decimals(3).suffix(" mm"));
        });
    };
    row(ui, "Package", |ui| {
        egui::ComboBox::from_id_salt("ipc-family").width(140.0).selected_text(format!("{:?}", w.family).to_uppercase()).show_ui(ui, |ui| {
            for (f, n) in [(Family::Chip, "Chip (R, C, L, D)"), (Family::Sop, "SOIC / SOP / TSSOP"), (Family::Qfp, "QFP"), (Family::Son, "SON / DFN"), (Family::Qfn, "QFN"), (Family::Bga, "BGA")] {
                ui.selectable_value(&mut w.family, f, n);
            }
        });
    });
    match w.family {
        Family::Chip => {
            row(ui, "Kind", |ui| {
                egui::ComboBox::from_id_salt("ipc-kind").width(140.0).selected_text(w.kind.clone()).show_ui(ui, |ui| {
                    for k in ["R", "C", "L", "D", "LED", "F"] {
                        ui.selectable_value(&mut w.kind, k.to_string(), k);
                    }
                });
            });
            row(ui, "EIA size", |ui| {
                egui::ComboBox::from_id_salt("ipc-size").width(140.0).selected_text(if w.size.is_empty() { "custom".to_string() } else { w.size.clone() }).show_ui(ui, |ui| {
                    for k in ["0201", "0402", "0603", "0805", "1206", "1210", "2010", "2512"] {
                        ui.selectable_value(&mut w.size, k.to_string(), k);
                    }
                    ui.selectable_value(&mut w.size, String::new(), "custom");
                });
            });
            if w.size.is_empty() {
                num(ui, "Body length", &mut w.body[0]);
                num(ui, "Body width", &mut w.body[1]);
                num(ui, "Terminal", &mut w.lead);
                num(ui, "Height", &mut w.height);
            }
        }
        Family::Bga => {
            num(ui, "Pitch", &mut w.pitch);
            num(ui, "Ball", &mut w.ball);
            row(ui, "Rows × cols", |ui| {
                ui.add(egui::DragValue::new(&mut w.rows).range(1..=40));
                ui.add(egui::DragValue::new(&mut w.cols).range(1..=40));
            });
            num(ui, "Body", &mut w.body[0]);
            num(ui, "Height", &mut w.height);
        }
        f => {
            row(ui, "Pins", |ui| {
                ui.add(egui::DragValue::new(&mut w.pins).range(0..=400));
            });
            num(ui, "Pitch", &mut w.pitch);
            num(ui, "Body length", &mut w.body[0]);
            num(ui, "Body width", &mut w.body[1]);
            if matches!(f, Family::Sop | Family::Qfp) {
                num(ui, "Lead span", &mut w.span[0]);
                if f == Family::Qfp {
                    num(ui, "Lead span y", &mut w.span[1]);
                }
            }
            num(ui, "Foot length", &mut w.lead);
            num(ui, "Lead width", &mut w.lead_w);
            num(ui, "Height", &mut w.height);
            num(ui, "Exposed pad w", &mut w.ep[0]);
            num(ui, "Exposed pad h", &mut w.ep[1]);
        }
    }
    row(ui, "Density", |ui| {
        for (d, tip) in [('M', "Most: big fillets, for hand soldering and rework"), ('N', "Nominal"), ('L', "Least: dense boards")] {
            ui.selectable_value(&mut w.density, d, d.to_string()).on_hover_text(tip);
        }
    });
    let made = crate::pcb_ipc::generate(w);
    match &made {
        Ok(f) => {
            ui.label(RichText::new(format!("{} · {} pads", f.name, f.pads.len())).strong().size(12.5));
            if let Some(p) = f.pads.first() {
                ui.label(RichText::new(format!("pad 1 {:.2} × {:.2} mm at ({:.3}, {:.3})", p.size[0], p.size[1], p.at[0], p.at[1])).color(theme::muted()).size(11.5));
            }
        }
        Err(e) => {
            ui.label(RichText::new(e).color(theme::muted()).size(11.5));
        }
    }
    if let Ok(f) = made {
        ui.horizontal(|ui| {
            let mut add = ui.button("Add to library").clicked();
            let mut use_on = None;
            if let Some(i) = sel_part {
                let r = app.ws.pcbs[pi].parts[i].reference.clone();
                if ui.button(format!("Add and use on {r}")).clicked() {
                    add = true;
                    use_on = Some(i);
                }
            }
            if add {
                let name = f.name.clone();
                crate::pcb_kicad::add_to(&mut app.ws.pcbs[pi], crate::pcb_kicad::Imported { footprints: vec![f], ..Default::default() });
                if let Some(i) = use_on {
                    app.ws.pcbs[pi].parts[i].footprint = name.clone();
                }
                app.flash_info(format!("{name} is in the library"), Some(Duration::from_secs(3)));
            }
        });
    }
}

fn selection_ui(app: &mut App, ui: &mut Ui, s: Sel) {
    let Some(pi) = index(app) else { return };
    let board = app.pcb.view == View::Board;
    let fps = pcb_lib::footprint_names();
    let lib_keys: Vec<String> = app.ws.pcbs[pi].symbols.iter().map(|s| s.key.clone()).collect();
    let p = &mut app.ws.pcbs[pi];
    match s {
        Sel::Part(i) if i < p.parts.len() => {
            let kind = pcb_lib::symbol_kind(&p.parts[i].symbol);
            let sim_model = crate::circuit::model_params(&p.parts[i], &p.sim);
            ui.label(RichText::new(kind.map(|k| k.name).unwrap_or(p.parts[i].symbol.rsplit(':').next().unwrap_or("Part"))).strong());
            let part = &mut p.parts[i];
            row(ui, "Reference", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.reference).desired_width(120.0));
            });
            row(ui, "Value", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.value).desired_width(120.0));
            });
            row(ui, "Type", |ui| {
                egui::ComboBox::from_id_salt("pcb-sym").width(140.0).selected_text(kind.map(|k| k.name).unwrap_or(part.symbol.as_str()).to_string()).height(320.0).show_ui(ui, |ui| {
                    for k in pcb_lib::SYMBOLS {
                        ui.selectable_value(&mut part.symbol, k.key.to_string(), k.name);
                    }
                    for s in &lib_keys {
                        ui.selectable_value(&mut part.symbol, s.clone(), s);
                    }
                })
                .response
                .on_hover_text("The schematic symbol (and simulation model); its pins follow the footprint");
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
            row(ui, "Package", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.package).desired_width(120.0).hint_text("supplier's name"));
            });
            if matches!(part.symbol.as_str(), "U" | "J") || !part.pin_names.is_empty() {
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
            } else {
                row(ui, "Position", |ui| {
                    // + 0.0 shows -0 as 0
                    let mut x = [part.sch[0] + 0.0, part.sch[1] + 0.0];
                    let a = ui.add(egui::DragValue::new(&mut x[0]).speed(GRID).suffix(" mm").max_decimals(2));
                    let b = ui.add(egui::DragValue::new(&mut x[1]).speed(GRID).suffix(" mm").max_decimals(2));
                    if a.changed() || b.changed() {
                        part.sch = pcb::snap(x, GRID);
                    }
                });
                row(ui, "Rotation", |ui| {
                    for (q, t) in [(0u8, "0°"), (1, "90°"), (2, "180°"), (3, "270°")] {
                        ui.selectable_value(&mut part.sch_rot, q, t);
                    }
                });
                row(ui, "Mirror", |ui| {
                    ui.checkbox(&mut part.sch_mirror, "");
                });
            }
            ui.add_space(4.0);
            ui.label(RichText::new("BOM").color(theme::muted()).size(11.0).strong());
            row(ui, "MPN", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.mpn).desired_width(120.0).hint_text("manufacturer part no."));
            });
            row(ui, "Manufacturer", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.manufacturer).desired_width(120.0));
            });
            row(ui, "Description", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.description).desired_width(120.0));
            });
            row(ui, "Digi-Key", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.digikey).desired_width(120.0).hint_text("e.g. 311-10.0KCRCT-ND"));
                if (!part.digikey.is_empty() || !part.mpn.is_empty()) && ui.small_button("↗").on_hover_text("Open on digikey.com").clicked() {
                    crate::printing::open_url(&crate::digikey::product_url(part));
                }
            });
            row(ui, "Datasheet", |ui| {
                ui.add(egui::TextEdit::singleline(&mut part.datasheet).desired_width(120.0).hint_text("link"));
                if part.datasheet.starts_with("http") && ui.small_button("↗").on_hover_text("Open the datasheet").clicked() {
                    crate::printing::open_url(&part.datasheet);
                }
            });
            let mut gone = None;
            for (k, (name, val)) in part.fields.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(name).desired_width(92.0).text_color(theme::muted()));
                    ui.add(egui::TextEdit::singleline(val).desired_width(100.0));
                    if ui.small_button("×").on_hover_text("Remove this field").clicked() {
                        gone = Some(k);
                    }
                });
            }
            if let Some(k) = gone {
                part.fields.remove(k);
            }
            ui.horizontal(|ui| {
                let (n, v) = &mut app.pcb.field_new;
                ui.add(egui::TextEdit::singleline(n).desired_width(92.0).hint_text("New field"));
                ui.add(egui::TextEdit::singleline(v).desired_width(100.0).hint_text("value"));
                if ui.add_enabled(!n.trim().is_empty(), egui::Button::new("+").small()).on_hover_text("Add a BOM field (Tolerance, Voltage, LCSC …)").clicked() {
                    part.fields.push((n.trim().to_string(), v.trim().to_string()));
                    n.clear();
                    v.clear();
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("SIMULATION").color(theme::muted()).size(11.0).strong());
            let mut on = !part.no_sim;
            if ui.checkbox(&mut on, "Simulate this part").on_hover_text("Off: the part is left out of the circuit (its pins stay open)").changed() {
                part.no_sim = !on;
            }
            match &sim_model {
                Ok((desc, params)) => {
                    ui.label(RichText::new(format!("Model: {desc}")).size(11.5));
                    if !params.is_empty() {
                        ui.label(RichText::new("Leave a box empty for the model's own value").color(theme::muted()).size(11.0));
                    }
                    for (key, label, unit, v) in params {
                        row(ui, label, |ui| {
                            let mut text = part.sim.get(*key).cloned().unwrap_or_default();
                            let def = crate::circuit::si(*v, unit);
                            let ok = text.is_empty() || crate::circuit::parse_si_lenient(&text).is_some();
                            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(86.0).hint_text(def.trim()).text_color_opt((!ok).then(theme::error))).on_hover_text(format!("{label} ({key}); the model gives {}", def.trim()));
                            if r.changed() {
                                if text.trim().is_empty() {
                                    part.sim.remove(*key);
                                } else {
                                    part.sim.insert(key.to_string(), text);
                                }
                            }
                            if !unit.is_empty() {
                                ui.label(RichText::new(*unit).color(theme::muted()).size(11.0));
                            }
                        });
                    }
                    // overrides for keys this model doesn't have (after a type change)
                    let stale: Vec<String> = part.sim.keys().filter(|k| !params.iter().any(|p| p.0 == k.as_str())).cloned().collect();
                    if !stale.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("Not in this model: {}", stale.join(", "))).color(theme::error()).size(11.0));
                            if ui.small_button("Remove").clicked() {
                                for k in &stale {
                                    part.sim.remove(k);
                                }
                            }
                        });
                    }
                    if !part.sim.is_empty() && ui.small_button("Reset to the model's values").clicked() {
                        part.sim.clear();
                    }
                }
                Err(e) => {
                    ui.label(RichText::new(e).color(theme::error()).size(11.5));
                }
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
            row(ui, "Pads", |ui| {
                let name = |c: pcb::PadConnect| match c {
                    pcb::PadConnect::ThtRelief => "Relief on through-hole",
                    pcb::PadConnect::Relief => "Relief on all",
                    pcb::PadConnect::Solid => "Solid",
                };
                egui::ComboBox::from_id_salt("pcb-zone-pads").selected_text(name(z.pads)).show_ui(ui, |ui| {
                    for c in [pcb::PadConnect::ThtRelief, pcb::PadConnect::Relief, pcb::PadConnect::Solid] {
                        ui.selectable_value(&mut z.pads, c, name(c));
                    }
                })
                .response
                .on_hover_text("Thermal reliefs (a gap and four spokes) keep the pour from soaking up the soldering iron's heat");
            });
            let v = |ui: &mut Ui, label: &str, x: &mut f64, tip: &str| {
                row(ui, label, |ui| {
                    ui.add(egui::DragValue::new(x).speed(0.01).range(0.0..=5.0).max_decimals(2).custom_formatter(|x, _| if x == 0.0 { "auto".into() } else { format!("{x:.2} mm") })).on_hover_text(tip);
                });
            };
            v(ui, "Clearance", &mut z.clearance, "Gap to other nets; auto takes the net's clearance");
            if z.pads != pcb::PadConnect::Solid {
                v(ui, "Relief gap", &mut z.relief_gap, "The gap round a relieved pad; auto is 0.3 mm");
                v(ui, "Spoke", &mut z.spoke, "Thermal spoke width; auto is the net's track width");
            }
        }
        Sel::Keepout(i) if i < p.board.keepouts.len() => {
            let k = &mut p.board.keepouts[i];
            ui.label(RichText::new("Keepout").strong());
            row(ui, "Layer", |ui| {
                ui.selectable_value(&mut k.layer, None, "Both");
                ui.selectable_value(&mut k.layer, Some(Layer::Top), "Top");
                ui.selectable_value(&mut k.layer, Some(Layer::Bottom), "Bottom");
            });
            ui.checkbox(&mut k.tracks, "No tracks");
            ui.checkbox(&mut k.vias, "No vias");
            ui.checkbox(&mut k.pours, "No pours");
            ui.checkbox(&mut k.parts, "No parts");
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
    // only an outline made by rect_outline can be resized here; others are kept as they are
    let rect = b.rect_radius();
    if let Some(r0) = rect {
        let mut r = r0;
        let mut changed = false;
        row(ui, "Size", |ui| {
            changed |= ui.add(egui::DragValue::new(&mut w).speed(0.5).range(5.0..=1000.0).clamp_existing_to_range(false).suffix(" mm").max_decimals(2)).changed();
            ui.label("×");
            changed |= ui.add(egui::DragValue::new(&mut h).speed(0.5).range(5.0..=1000.0).clamp_existing_to_range(false).suffix(" mm").max_decimals(2)).changed();
        });
        row(ui, "Corners", |ui| {
            changed |= ui.add(egui::DragValue::new(&mut r).speed(0.1).range(0.0..=50.0).clamp_existing_to_range(false).suffix(" mm").max_decimals(2)).on_hover_text("Corner radius").changed();
        });
        if changed {
            // pours over the whole board follow it; pours drawn by hand stay
            b.resize(w, h, r);
        }
    } else {
        row(ui, "Size", |ui| {
            ui.label(format!("{w:.2} × {h:.2} mm (custom outline)"));
        });
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
        num(ui, "Hole gap", &mut r.hole_gap, "Hole edge to hole edge");
        let opt = |ui: &mut Ui, label: &str, v: &mut f64, tip: &str| {
            row(ui, label, |ui| {
                ui.add(egui::DragValue::new(v).speed(0.01).range(0.0..=50.0).suffix(" mm").max_decimals(2).custom_formatter(|x, _| if x == 0.0 { "off".into() } else { format!("{x:.2} mm") })).on_hover_text(tip);
            });
        };
        opt(ui, "Silk gap", &mut r.silk_gap, "Silkscreen to exposed pad copper (silk on pads is always flagged)");
        opt(ui, "Max. height", &mut r.max_height, "Tallest part allowed above or below the board (an enclosure's limit)");
        opt(ui, "Body gap", &mut r.body_gap, "Gap kept between parts' 3D bodies (colliding bodies are always flagged)");
    });
    ui.collapsing(format!("Net classes ({})", b.rules.classes.len()), |ui| {
        ui.label(RichText::new("Nets in a class get its clearance, track width and via when routed and checked; 0 keeps the board rule. A trailing * matches a prefix (M1_*).").color(theme::muted()).size(11.5));
        let mut remove = None;
        for (ci, c) in b.rules.classes.iter_mut().enumerate() {
            ui.push_id(("pcb-class", ci), |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(110.0).hint_text("Class name"));
                    if ui.small_button("×").on_hover_text("Remove the class").clicked() {
                        remove = Some(ci);
                    }
                });
                let mut text = c.nets.join(", ");
                row(ui, "Nets", |ui| {
                    if ui.add(egui::TextEdit::singleline(&mut text).desired_width(150.0).hint_text("VCC, M1_*")).on_hover_text("Net names, comma separated").changed() {
                        c.nets = text.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                    }
                });
                let v = |ui: &mut Ui, label: &str, x: &mut f64, unit: &'static str| {
                    row(ui, label, |ui| {
                        ui.add(egui::DragValue::new(x).speed(0.005).range(0.0..=20.0).max_decimals(3).custom_formatter(move |x, _| if x == 0.0 { "board".into() } else { format!("{x:.3} {unit}") }));
                    });
                };
                v(ui, "Clearance", &mut c.clearance, "mm");
                v(ui, "Track", &mut c.track, "mm");
                v(ui, "Via", &mut c.via_d, "mm");
                v(ui, "Via drill", &mut c.via_drill, "mm");
            });
        }
        if let Some(ci) = remove {
            b.rules.classes.remove(ci);
        }
        if ui.button("Add net class").clicked() {
            let n = b.rules.classes.len() + 1;
            b.rules.classes.push(pcb::NetClass { name: format!("Class{n}"), ..Default::default() });
        }
    });
    ui.collapsing("Checks", |ui| {
        ui.label(RichText::new("Untick a check to leave it out of the design and electrical rule checks.").color(theme::muted()).size(11.5));
        for (id, what) in pcb::CHECKS {
            let mut on = b.rules.checks(id);
            if ui.checkbox(&mut on, *what).changed() {
                b.rules.off.retain(|o| o != *id);
                if !on {
                    b.rules.off.push(id.to_string());
                }
            }
        }
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
            place_parts(app, true);
        }
        if ui.button("Remove tracks").on_hover_text("Delete every track and via (Ctrl+Z brings them back)").clicked() {
            edit(app, |p| {
                p.board.tracks.clear();
                p.board.vias.clear();
            });
        }
    });
}

fn lengths_ui(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    ui.checkbox(&mut app.pcb.walkaround, "Walkaround routing").on_hover_text("While routing by hand, the track flows round other nets' copper to the cursor");
    let nets: Vec<String> = app.pcb.cache.nl.nets.iter().map(|n| n.name.clone()).collect();
    let mut route_pair: Option<pcb::DiffPair> = None;
    let mut match_rule: Option<pcb::LengthRule> = None;
    {
        let p = &mut app.ws.pcbs[pi];
        let len = |p: &Pcb, n: &str| crate::pcb_tune::net_length(p, n);
        ui.label(RichText::new("Differential pairs").strong().size(12.5));
        let mut remove = None;
        for k in 0..p.board.rules.pairs.len() {
            let (lp, ln) = (len(p, &p.board.rules.pairs[k].p), len(p, &p.board.rules.pairs[k].n));
            let d = &mut p.board.rules.pairs[k];
            ui.push_id(("pcb-pair", k), |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(70.0));
                    for (v, id) in [(&mut d.p, "p"), (&mut d.n, "n")] {
                        egui::ComboBox::from_id_salt(id).width(70.0).selected_text(v.clone()).show_ui(ui, |ui| {
                            for n in &nets {
                                ui.selectable_value(v, n.clone(), n);
                            }
                        });
                    }
                    if ui.small_button("×").clicked() {
                        remove = Some(k);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.width).speed(0.005).range(0.0..=5.0).max_decimals(3).custom_formatter(|x, _| if x == 0.0 { "w auto".into() } else { format!("w {x:.3}") }));
                    ui.add(egui::DragValue::new(&mut d.gap).speed(0.005).range(0.0..=5.0).max_decimals(3).custom_formatter(|x, _| if x == 0.0 { "gap auto".into() } else { format!("gap {x:.3}") }));
                    if ui.button("Route").on_hover_text("Route the two nets side by side at the gap").clicked() {
                        route_pair = Some(d.clone());
                    }
                });
                if lp > 0.0 || ln > 0.0 {
                    ui.label(RichText::new(format!("{:.2} / {:.2} mm (skew {:.2})", lp, ln, (lp - ln).abs())).color(theme::muted()).size(11.5));
                }
            });
        }
        if let Some(k) = remove {
            p.board.rules.pairs.remove(k);
        }
        if ui.button("Add pair").on_hover_text("Nets named …_P/…_N or …+/…- are picked up").clicked() {
            // guess a pair from net names
            let guess = nets.iter().find_map(|a| {
                for (sp, sn) in [("_P", "_N"), ("+", "-"), ("P", "N"), ("_DP", "_DN")] {
                    if let Some(stem) = a.strip_suffix(sp) {
                        let b = format!("{stem}{sn}");
                        if nets.contains(&b) && !p.board.rules.pairs.iter().any(|d| d.p == *a) {
                            return Some((stem.trim_end_matches('_').to_string(), a.clone(), b));
                        }
                    }
                }
                None
            });
            let (name, pn, nn) = guess.unwrap_or_else(|| (format!("Pair{}", p.board.rules.pairs.len() + 1), nets.first().cloned().unwrap_or_default(), nets.get(1).cloned().unwrap_or_default()));
            p.board.rules.pairs.push(pcb::DiffPair { name, p: pn, n: nn, ..Default::default() });
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Length rules").strong().size(12.5));
        let mut remove = None;
        for k in 0..p.board.rules.lengths.len() {
            let r = p.board.rules.lengths[k].clone();
            let lens: Vec<(String, f64)> = crate::pcb_tune::rule_nets(&app.pcb.cache.nl, &r).into_iter().map(|n| (n.clone(), len(p, &n))).collect();
            let r = &mut p.board.rules.lengths[k];
            ui.push_id(("pcb-len", k), |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(70.0));
                    let mut text = r.nets.join(", ");
                    if ui.add(egui::TextEdit::singleline(&mut text).desired_width(110.0).hint_text("D0, D1, CLK")).changed() {
                        r.nets = text.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                    }
                    if ui.small_button("×").clicked() {
                        remove = Some(k);
                    }
                });
                ui.horizontal(|ui| {
                    let f = |label: &'static str| move |x: f64, _| if x == 0.0 { format!("{label} –") } else { format!("{label} {x:.2}") };
                    ui.add(egui::DragValue::new(&mut r.min).speed(0.1).range(0.0..=1000.0).custom_formatter(f("min")));
                    ui.add(egui::DragValue::new(&mut r.max).speed(0.1).range(0.0..=1000.0).custom_formatter(f("max")));
                    ui.add(egui::DragValue::new(&mut r.tolerance).speed(0.01).range(0.0..=100.0).custom_formatter(f("match ±"))).on_hover_text("Matched length: every net within this of the longest");
                    if ui.button("Tune").on_hover_text("Lengthen the shorter nets with meanders").clicked() {
                        match_rule = Some(r.clone());
                    }
                });
                for (n, l) in &lens {
                    ui.label(RichText::new(format!("{n}: {l:.2} mm")).color(theme::muted()).size(11.5));
                }
            });
        }
        if let Some(k) = remove {
            p.board.rules.lengths.remove(k);
        }
        if ui.button("Add length rule").clicked() {
            let n = p.board.rules.lengths.len() + 1;
            p.board.rules.lengths.push(pcb::LengthRule { name: format!("Length{n}"), ..Default::default() });
        }
    }
    if let Some(d) = route_pair {
        let mut res = None;
        edit(app, |p| res = Some(crate::pcb_route::route_pair(p, &d)));
        match res {
            Some(Ok(o)) => app.flash_info(format!("Routed pair {} on {}: {:.2} and {:.2} mm", d.name, o.layer.label().to_lowercase(), o.p_len, o.n_len), Some(Duration::from_secs(4))),
            Some(Err(e)) => app.flash(e, ToastKind::Error),
            None => {}
        }
    }
    if let Some(r) = match_rule {
        let mut res = vec![];
        edit(app, |p| res = crate::pcb_tune::match_lengths(p, &r));
        let worst = res.iter().map(|x| x.2).fold(0.0, f64::max) - res.iter().map(|x| x.2).fold(f64::INFINITY, f64::min);
        app.flash_info(format!("Tuned {}: lengths now within {:.2} mm", r.name, if worst.is_finite() { worst } else { 0.0 }), Some(Duration::from_secs(4)));
    }
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
        let text = RichText::new(format!("{} {}", if is.error { "●" } else { "○" }, is.message)).color(col).size(12.0);
        let text = if app.pcb.issue_shown == Some((vi(app.pcb.view), k)) { text.strong().underline() } else { text };
        let r = ui.add(egui::Label::new(text).sense(Sense::click()));
        if r.on_hover_text("Show it").clicked() {
            let v = vi(app.pcb.view);
            app.pcb.center[v] = is.at;
            app.pcb.zoom[v] = app.pcb.zoom[v].max(12.0);
            app.pcb.fit_size[v] = None;
            app.pcb.issue_shown = Some((v, k));
        }
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
        ("Simulate circuit", "pcbSim", |a| set_view(a, View::Simulate)),
        ("Run circuit simulation", "play", crate::circuit_ui::run),
        ("Wire", "pcbWire", |a| {
            set_view(a, View::Schematic);
            set_tool(a, Tool::Wire)
        }),
        ("Net label", "pcbLabel", |a| {
            set_view(a, View::Schematic);
            set_tool(a, Tool::Label(LabelKind::Net))
        }),
        ("Route tracks", "pcbRoute", |a| {
            set_view(a, View::Board);
            set_tool(a, Tool::Route)
        }),
        ("Autoroute", "pcbAutoroute", autoroute),
        ("Place parts on the board", "pcbPlace", auto_place),
        ("Design rule check", "pcbDrc", run_drc),
        ("Export Gerbers…", "export", export_fab),
        ("Run output job…", "export", run_output_job),
        ("3D board (Part Studio)", "cube", to_3d),
    ]
}

// ---------------------------------------------------------------- viewport

pub(crate) struct Xf {
    pub(crate) rect: egui::Rect,
    pub(crate) zoom: f32,
    center: P,
}

impl Xf {
    pub(crate) fn s(&self, p: P) -> Pos2 {
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
    let k = pcb_lib::kind_info(key)?;
    Some(pcb::Part { reference: format!("{}?", k.prefix), value: k.value, symbol: k.key, footprint: k.footprint, sch: at, sch_rot: rot, sch_mirror: mirror, ..Default::default() })
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

/// Hit-test the board at a world point (the active layer first). Copper on hidden layers and
/// hidden pours can't be picked. A click on a part's pad picks the part, not the tracks that end
/// on the pad.
fn board_hit(p: &Pcb, pads: &[WPad], at: P, tol: f64, layer: Layer, show: &BoardView) -> Option<Sel> {
    let shown = |l: Layer| if l == Layer::Top { show.top } else { show.bottom };
    for (i, v) in p.board.vias.iter().enumerate() {
        if pcb::dist(v.at, at) <= v.d / 2.0 + tol {
            return Some(Sel::Via(i));
        }
    }
    for l in [layer, layer.other()].into_iter().filter(|l| shown(*l)) {
        if let Some(pad) = pads.iter().find(|x| x.layer.is_none_or(|pl| pl == l) && x.cu().contains(at)) {
            return Some(Sel::Part(pad.part));
        }
    }
    for l in [layer, layer.other()].into_iter().filter(|l| shown(*l)) {
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
    for (i, z) in p.board.zones.iter().enumerate().filter(|(_, z)| show.zones && shown(z.layer)) {
        let n = z.outline.len();
        if (0..n).any(|k| pcb::seg_point(z.outline[k], z.outline[(k + 1) % n], at) < tol * 2.0) {
            return Some(Sel::Zone(i));
        }
    }
    for (i, k) in p.board.keepouts.iter().enumerate() {
        let n = k.outline.len();
        if (0..n).any(|j| pcb::seg_point(k.outline[j], k.outline[(j + 1) % n], at) < tol * 2.0) || (n >= 3 && pcb::inside(&k.outline, at)) {
            return Some(Sel::Keepout(i));
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

/// The grid route corners snap to, in mm.
const ROUTE_GRID: f64 = 0.05;

/// Where a route click at `h` puts the next corner after `start`: on the grid, but lined up with
/// `start` (straight or at 45°) when within `tol` of that, so off-grid pads get no tiny stubs.
fn route_end(start: P, h: P, grid: f64, tol: f64) -> P {
    let mut e = pcb::snap(h, grid);
    let tol = tol.max(grid);
    let (dx, dy) = (h[0] - start[0], h[1] - start[1]);
    if dx.abs() <= tol {
        e[0] = start[0];
    }
    if dy.abs() <= tol {
        e[1] = start[1];
    }
    if dx.abs() > tol && dy.abs() > tol && (dx.abs() - dy.abs()).abs() <= tol {
        // a 45° diagonal: keep the snapped x, put y on the diagonal
        let ex = e[0] - start[0];
        e[1] = start[1] + ex.abs() * dy.signum();
    }
    e
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
    if app.pcb.tool == Tool::Keepout {
        let mut idx = 0;
        edit(app, |p| {
            p.board.keepouts.push(pcb::Keepout { outline: pcb::ccw(pts), ..Default::default() });
            idx = p.board.keepouts.len() - 1;
        });
        set_tool(app, Tool::Select);
        app.pcb.sel = Some(Sel::Keepout(idx));
        return;
    }
    let layer = app.pcb.layer;
    let net = app.pcb.zone_net.clone();
    if net.is_empty() {
        app.flash_info("A pour needs a net: connect some pins in the schematic first", Some(Duration::from_secs(3)));
        return;
    }
    edit(app, |p| p.board.zones.push(Zone { net, layer, outline: pcb::ccw(pts), ..Default::default() }));
    set_tool(app, Tool::Select);
    warn_empty_pour(app);
}

pub fn viewport(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    refresh(app);
    let sim = app.pcb.view == View::Simulate;
    let full = ui.available_rect_before_wrap();
    let (rect, plot_rect) = if sim { crate::circuit_ui::split(app, full) } else { (full, None) };
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    app.viewport_rect = rect;
    let k = vi(app.pcb.view);
    let board = app.pcb.view == View::Board;
    // fit on first show, and again while the viewport is still being laid out
    if app.pcb.fit_size[k].is_some_and(|s| (s - rect.size()).length() > 0.5) {
        app.pcb.fitted[k] = false;
    }
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
        app.pcb.fit_size[k] = Some(rect.size());
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
            app.pcb.fit_size[k] = None;
        }
    }
    if resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged_by(egui::PointerButton::Secondary)) {
        app.pcb.fit_size[k] = None;
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
    } else if sim {
        crate::circuit_ui::sim_input(app, pi, hover, tol, clicked);
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
        if sim {
            crate::circuit_ui::overlay(app, pi, &painter, &xf);
        }
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
    if let Some(r) = plot_rect {
        crate::circuit_ui::plot(app, ui, r);
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
                    let dk = app.pcb.dk.placing.clone();
                    let mut err = None;
                    let mut new = None;
                    edit(app, |p| match p.add_part(&key, None, None, h) {
                        Ok(i) => {
                            p.parts[i].sch_rot = rot;
                            p.parts[i].sch_mirror = mir;
                            if let Some(d) = &dk {
                                crate::digikey_ui::fill_placed(&mut p.parts[i], d);
                                let at = p.free_spot(&p.parts[i]);
                                p.parts[i].at = at;
                            }
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
        Tool::NoConnect => {
            if let (true, Some(h)) = (clicked, hover) {
                let p = &app.ws.pcbs[pi];
                let at = sch_snap(p, h, tol * 1.5);
                if p.sch_pins().iter().any(|x| pcb::dist(x.at, at) < 1e-6) {
                    edit(app, |p| {
                        let n = p.no_connects.len();
                        p.no_connects.retain(|q| pcb::dist(*q, at) > 1e-6);
                        if p.no_connects.len() == n {
                            p.no_connects.push(at);
                        }
                    });
                } else {
                    app.flash_info("Click a pin's end to mark it", Some(Duration::from_secs(2)));
                }
            }
            if right {
                set_tool(app, Tool::Select);
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
                        // (the undo step is recorded when the drag ends, if it moved anything)
                        app.pcb.drag = Some(Drag { from: at, orig: p.parts[i].sch, ends, before: p.clone() });
                    } else if let Some(Sel::Label(i)) = sch_hit(p, at, tol) {
                        app.pcb.sel = Some(Sel::Label(i));
                        // (the undo step is recorded when the drag ends, if it moved anything)
                        app.pcb.drag = Some(Drag { from: at, orig: p.labels[i].at, ends: vec![], before: p.clone() });
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
                finish_drag(app);
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
/// While routing, the walkaround path from the last corner to the cursor (round other nets'
/// copper), recomputed when the cursor moves.
fn walk_preview(app: &mut App, pi: usize, h: P, tol: f64) {
    let Some(r) = &app.pcb.route else {
        app.pcb.walk = None;
        app.pcb.walk_path = None;
        return;
    };
    let (net, width, layer, from) = (r.net.clone(), r.width, r.layer, *r.pts.last().unwrap());
    let stale = app.pcb.walk.as_ref().is_none_or(|w| w.1 != net || (w.2 - width).abs() > 1e-9 || w.3 != app.pcb.cache_gen);
    if stale {
        app.pcb.walk = Some((crate::pcb_route::Walk::new(&app.ws.pcbs[pi], &net, width), net.clone(), width, app.pcb.cache_gen));
        app.pcb.walk_path = None;
    }
    // the same point a click would end on: copper of the net, else on the grid lined up with the last point
    let c = &app.pcb.cache;
    let to = pcb::copper_at(&app.ws.pcbs[pi], &c.nl, &c.pads, h, layer, tol).filter(|x| x.net == net && pcb::dist(x.at, from) > 1e-6).map(|x| x.at).unwrap_or_else(|| route_end(from, h, ROUTE_GRID, tol));
    if app.pcb.walk_path.as_ref().is_some_and(|w| w.0 == from && pcb::dist(w.1, to) < 0.05 && w.2 == layer) {
        return;
    }
    let path = app.pcb.walk.as_ref().and_then(|w| w.0.path(from, to, layer));
    app.pcb.walk_path = Some((from, to, layer, path));
}

fn board_input(app: &mut App, pi: usize, resp: &egui::Response, xf: &Xf, hover: Option<P>, tol: f64, clicked: bool, double: bool, right: bool) {
    let tool = app.pcb.tool.clone();
    match tool {
        Tool::Route => {
            let Some(h) = hover else { return };
            if app.pcb.walkaround {
                walk_preview(app, pi, h, tol);
            }
            if clicked {
                let p = &app.ws.pcbs[pi];
                let c = &app.pcb.cache;
                let layer = app.pcb.layer;
                let shown = |l: Layer| if l == Layer::Top { app.pcb.show.top } else { app.pcb.show.bottom };
                match &app.pcb.route {
                    // the second click of the double-click that ended a route
                    None if double => {}
                    None => {
                        // start on a pad, track or via of a shown layer
                        let hit = [layer, layer.other()].into_iter().filter(|l| shown(*l)).find_map(|l| pcb::copper_at(p, &c.nl, &c.pads, h, l, tol));
                        if let Some(hit) = hit {
                            // a surface mount pad or track on the other side starts there
                            let l = hit.layer.unwrap_or(layer);
                            app.pcb.layer = l;
                            let w = p.track_width(&c.nl, &hit.net);
                            app.pcb.highlight = if hit.net.is_empty() { None } else { Some(hit.net.clone()) };
                            app.pcb.route = Some(Route { net: hit.net, layer: l, pts: vec![hit.at], width: w });
                        } else {
                            app.flash_info("Start on a pad, track or via", Some(Duration::from_secs(2)));
                        }
                    }
                    Some(r) => {
                        let start = *r.pts.last().unwrap();
                        let (net, rl, w) = (r.net.clone(), r.layer, r.width);
                        // ending on copper of the same net finishes the route there
                        let target = pcb::copper_at(p, &c.nl, &c.pads, h, rl, tol).filter(|x| x.net == net && pcb::dist(x.at, start) > 1e-6);
                        // copper of another net under the cursor would be a short
                        let other = pcb::copper_at(p, &c.nl, &c.pads, h, rl, 0.0).filter(|x| x.net != net || (x.pad && x.net.is_empty()));
                        if let (None, Some(o)) = (&target, other) {
                            let what = if o.net.is_empty() { "a pad with no net".to_string() } else { format!("{} copper", o.net) };
                            let msg = format!("That is {what}: a {} track can't end there", if net.is_empty() { "no-net" } else { &net });
                            app.flash_info(msg, Some(Duration::from_secs(3)));
                            return;
                        }
                        let end = target.as_ref().map(|x| x.at).unwrap_or_else(|| route_end(start, h, ROUTE_GRID, tol));
                        // the walkaround path when there is one to this point, else a 45° bend
                        let walked = app.pcb.walk_path.as_ref().filter(|w| app.pcb.walkaround && w.0 == start && w.2 == rl && pcb::dist(w.1, end) < 0.1).and_then(|w| w.3.clone());
                        let path = match walked {
                            Some(mut w) => {
                                *w.last_mut().unwrap() = end;
                                w
                            }
                            None => route45(start, end),
                        };
                        edit(app, |p| {
                            for s in path.windows(2).filter(|s| pcb::dist(s[0], s[1]) > 1e-6) {
                                p.board.tracks.push(Track { a: s[0], b: s[1], width: w, layer: rl, net: net.clone() });
                            }
                        });
                        if target.is_some() || double {
                            app.pcb.route = None;
                            app.pcb.highlight = None;
                        } else if let Some(r) = &mut app.pcb.route {
                            if pcb::dist(start, end) > 1e-6 {
                                r.pts.push(end);
                            }
                        }
                    }
                }
            }
            if right {
                if app.pcb.route.is_some() {
                    app.pcb.route = None;
                    app.pcb.highlight = None;
                } else {
                    set_tool(app, Tool::Select);
                }
            }
        }
        Tool::Zone | Tool::Keepout => {
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
                // what was under the pointer when the button went down, not where it is now
                let origin = resp.ctx.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos());
                if let Some(at) = origin.map(|s| xf.w(s)) {
                    match board_hit(p, &app.pcb.cache.pads, at, tol, app.pcb.layer, &app.pcb.show) {
                        Some(Sel::Part(i)) => {
                            app.pcb.sel = Some(Sel::Part(i));
                            // (the undo step is recorded when the drag ends, if it moved anything)
                            app.pcb.drag = Some(Drag { from: at, orig: p.parts[i].at, ends: vec![], before: p.clone() });
                        }
                        Some(Sel::Hole(i)) => {
                            app.pcb.sel = Some(Sel::Hole(i));
                            // (the undo step is recorded when the drag ends, if it moved anything)
                            app.pcb.drag = Some(Drag { from: at, orig: p.board.holes[i].at, ends: vec![], before: p.clone() });
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
                finish_drag(app);
            }
            if clicked {
                let p = &app.ws.pcbs[pi];
                let c = &app.pcb.cache;
                app.pcb.sel = hover.and_then(|h| board_hit(p, &c.pads, h, tol, app.pcb.layer, &app.pcb.show));
                // clicking copper shows its net
                app.pcb.highlight = hover.and_then(|h| pcb::copper_at(p, &c.nl, &c.pads, h, app.pcb.layer, tol)).map(|x| x.net).filter(|n| !n.is_empty());
            }
        }
    }
}

fn sch_overlay(app: &App, pi: usize, painter: &egui::Painter, xf: &Xf) {
    let p = &app.ws.pcbs[pi];
    let hl = Color32::from_rgb(60, 130, 230);
    if let (Some(issues), View::Schematic) = (&app.pcb.erc, app.pcb.view) {
        for is in issues {
            let col = if is.error { Color32::from_rgb(230, 50, 50) } else { Color32::from_rgb(235, 170, 30) };
            painter.circle_stroke(xf.s(is.at), 7.0, Stroke::new(2.0, col));
        }
    }
    if app.pcb.tool == Tool::NoConnect {
        let h = app.pcb.hover;
        let c = xf.s(h);
        painter.line_segment([c + egui::vec2(-5.0, -5.0), c + egui::vec2(5.0, 5.0)], Stroke::new(1.5, hl));
        painter.line_segment([c + egui::vec2(-5.0, 5.0), c + egui::vec2(5.0, -5.0)], Stroke::new(1.5, hl));
    }
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
            // below the courtyard: the silkscreen reference usually sits above it
            painter.text(xf.s([lo[0], lo[1]]) + egui::vec2(0.0, 4.0), egui::Align2::LEFT_TOP, format!("{} {}", p.parts[i].reference, p.parts[i].value), egui::FontId::proportional(12.0), hl);
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
        Some(Sel::Keepout(i)) if i < p.board.keepouts.len() => {
            let pts: Vec<Pos2> = p.board.keepouts[i].outline.iter().map(|q| xf.s(*q)).collect();
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
        // the one picked in the list: a bold ring and cross hairs, drawn last so it sits on top
        if let Some(is) = app.pcb.issue_shown.filter(|x| x.0 == 1).and_then(|x| issues.get(x.1)) {
            let c = xf.s(is.at);
            let col = if is.error { Color32::from_rgb(255, 80, 80) } else { Color32::from_rgb(255, 200, 60) };
            painter.circle_stroke(c, 13.0, Stroke::new(3.0, col));
            for d in [egui::vec2(1.0, 0.0), egui::vec2(0.0, 1.0)] {
                painter.line_segment([c - d * 20.0, c - d * 9.0], Stroke::new(2.0, col));
                painter.line_segment([c + d * 9.0, c + d * 20.0], Stroke::new(2.0, col));
            }
        }
    }
    let h = app.pcb.hover;
    if let Some(r) = &app.pcb.route {
        let col = if r.layer == Layer::Top { pcb_export::colors::TOP } else { pcb_export::colors::BOTTOM };
        let col = Color32::from_rgba_unmultiplied(col[0], col[1], col[2], 170);
        let start = *r.pts.last().unwrap();
        let walked = app.pcb.walk_path.as_ref().filter(|w| app.pcb.walkaround && w.0 == start && w.2 == r.layer).and_then(|w| w.3.clone());
        let path = walked.unwrap_or_else(|| route45(start, route_end(start, h, ROUTE_GRID, (6.0 / xf.zoom) as f64)));
        let wpx = (r.width as f32 * xf.zoom).max(1.5);
        for s in path.windows(2) {
            painter.line_segment([xf.s(s[0]), xf.s(s[1])], Stroke::new(wpx, col));
        }
        painter.text(xf.s(h) + egui::vec2(12.0, -12.0), egui::Align2::LEFT_BOTTOM, format!("{} · {} · {} mm", r.net, r.layer.label(), r.width), egui::FontId::proportional(11.5), Color32::WHITE);
    }
    if matches!(app.pcb.tool, Tool::Zone | Tool::Keepout) && !app.pcb.zone_pts.is_empty() {
        let mut pts: Vec<Pos2> = app.pcb.zone_pts.iter().map(|q| xf.s(*q)).collect();
        pts.push(xf.s(pcb::snap(h, 0.1)));
        painter.add(egui::Shape::closed_line(pts, Stroke::new(1.5, hl)));
    }
    if app.pcb.tool == Tool::Hole {
        painter.circle_stroke(xf.s(pcb::snap(h, 0.1)), 1.6 * xf.zoom, Stroke::new(1.2, hl));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_corners_snap_and_line_up() {
        // an off-grid pad: a click a little off its row stays on the row, no 45° stub
        let pad = [16.2625, 20.95];
        let e = route_end(pad, [10.8405, 20.9889], ROUTE_GRID, 0.2);
        assert!((e[0] - 10.85).abs() < 1e-9 && e[1] == pad[1], "{e:?}");
        assert_eq!(route45(pad, e).len(), 2);
        // far from the row and column: the grid
        let e = route_end(pad, [12.013, 23.987], ROUTE_GRID, 0.2);
        assert!((e[0] - 12.0).abs() < 1e-9 && (e[1] - 24.0).abs() < 1e-9);
        // close to 45°: exactly diagonal
        let e = route_end(pad, [pad[0] + 3.02, pad[1] - 2.95], ROUTE_GRID, 0.2);
        assert!(((e[0] - pad[0]).abs() - (e[1] - pad[1]).abs()).abs() < 1e-9, "{e:?}");
        assert_eq!(route45(pad, e).len(), 2);
    }

    #[test]
    fn hidden_layers_and_pads_in_board_hit() {
        let mut p = pcb::tests::led_board();
        let nl = p.netlist();
        let pads = p.pads(&nl);
        let r1 = pads.iter().find(|x| x.part == 0 && x.num == "1").unwrap().at;
        // an autorouted track ending at the pad's centre: clicking the pad still picks the part
        p.board.tracks.push(Track { a: r1, b: [r1[0], r1[1] - 5.0], width: 0.25, layer: Layer::Top, net: "VCC".into() });
        let show = BoardView::default();
        assert_eq!(board_hit(&p, &pads, r1, 0.1, Layer::Top, &show), Some(Sel::Part(0)));
        let mid = [r1[0], r1[1] - 3.0];
        assert_eq!(board_hit(&p, &pads, mid, 0.1, Layer::Top, &show), Some(Sel::Track(0)));
        // with the top layer hidden the track can't be picked
        let hidden = BoardView { top: false, ..BoardView::default() };
        assert_eq!(board_hit(&p, &pads, mid, 0.1, Layer::Top, &hidden), None);
    }
}
