//! Command search (S, / or Ctrl+K): type part of any command's name in the current workspace and press
//! Enter to run it, like Fusion 360's S key.

use egui::{Key, RichText};

use crate::app::App;
use crate::cam::OpKind;
use crate::theme;
use crate::workspace::Tab;

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    query: String,
    sel: usize,
    /// Labels of the commands run lately, newest first.
    recent: Vec<String>,
    focus: bool,
}

#[derive(Clone)]
enum Action {
    Feature(&'static str),
    SketchTool(crate::sketcher::Tool),
    Constrain(&'static str),
    CamOp(OpKind),
    Tab(Tab),
    Run(fn(&mut App)),
}

#[derive(Clone)]
struct Cmd {
    label: String,
    group: &'static str,
    icon: String,
    keys: Option<String>,
    /// Extra words it is found by.
    words: &'static str,
    action: Action,
}

fn cmd(label: impl Into<String>, group: &'static str, icon: impl Into<String>, keys: Option<&str>, words: &'static str, action: Action) -> Cmd {
    Cmd { label: label.into(), group, icon: icon.into(), keys: keys.map(str::to_string), words, action }
}

/// Everything that can be run from where the user is.
fn commands(app: &App) -> Vec<Cmd> {
    let mut v = vec![];
    match app.tab {
        Tab::Studio(_) if app.sketch.is_some() => {
            for (t, icon, label, k) in crate::sketcher::TOOLS {
                v.push(cmd(*label, "Sketch", *icon, *k, "draw", Action::SketchTool(*t)));
            }
            for (key, label, k, _) in crate::sketcher::CONSTRAINTS {
                v.push(cmd(*label, "Constraint", *key, *k, "constrain", Action::Constrain(key)));
            }
            v.push(cmd("Finish sketch", "Sketch", "check", Some("Enter"), "done close", Action::Run(crate::sketcher::finish)));
            v.push(cmd("Finish and extrude", "Sketch", "extrude", Some("Shift+E"), "", Action::Run(|a| a.extrude_from_sketch())));
            v.push(cmd("Discard sketch changes", "Sketch", "close", None, "cancel", Action::Run(crate::sketcher::cancel)));
        }
        Tab::Studio(_) => {
            for k in crate::app::FEATURE_GROUPS.iter().flat_map(|g| g.iter().copied()) {
                v.push(cmd(crate::doc::kind_label(k), "Feature", k, crate::app::shortcut(k), "", Action::Feature(k)));
            }
            v.push(cmd("Measure", "View", "measure", Some("M"), "distance angle", Action::Run(|a| a.toggle_measure())));
            v.push(cmd(
                "X-ray",
                "View",
                "xray",
                None,
                "see through transparent",
                Action::Run(|a| {
                    a.xray = !a.xray;
                    a.scene_dirty = true;
                }),
            ));
            v.push(cmd(
                "Show dimensions",
                "View",
                "dimension",
                None,
                "dims",
                Action::Run(|a| {
                    a.show_dims = !a.show_dims;
                    a.scene_dirty = true;
                }),
            ));
            v.push(cmd("View normal to selection", "View", "normal", Some("N"), "look at", Action::Run(|a| a.view_normal_to_selection())));
            v.push(cmd("Import a model or drawing…", "File", "importFile", None, "step iges stl obj dxf svg", Action::Run(|a| a.import_step())));
            for (label, view, key) in [
                ("Iso view", "iso", Some("0")),
                ("Front view", "front", Some("1")),
                ("Top view", "top", Some("2")),
                ("Right view", "right", Some("3")),
                ("Back view", "back", None),
                ("Bottom view", "bottom", None),
                ("Left view", "left", None),
            ] {
                v.push(cmd(
                    label,
                    "View",
                    "isoView",
                    key,
                    "camera standard",
                    Action::Run(match view {
                        "front" => |a| set_view(a, crate::camera::StdView::Front),
                        "top" => |a| set_view(a, crate::camera::StdView::Top),
                        "right" => |a| set_view(a, crate::camera::StdView::Right),
                        "back" => |a| set_view(a, crate::camera::StdView::Back),
                        "bottom" => |a| set_view(a, crate::camera::StdView::Bottom),
                        "left" => |a| set_view(a, crate::camera::StdView::Left),
                        _ => |a| set_view(a, crate::camera::StdView::Iso),
                    }),
                ));
            }
            for (label, plane) in [("Section view: Top plane", 0u8), ("Section view: Front plane", 1), ("Section view: Right plane", 2)] {
                v.push(cmd(
                    label,
                    "View",
                    "section",
                    None,
                    "cut clip cross",
                    Action::Run(match plane {
                        0 => |a| set_section(a, 0),
                        1 => |a| set_section(a, 1),
                        _ => |a| set_section(a, 2),
                    }),
                ));
            }
            v.push(cmd(
                "Section view through the selection",
                "View",
                "section",
                None,
                "cut clip cross face plane",
                Action::Run(|a| match a.section_through_selection() {
                    Some(s) => {
                        a.section = Some(s);
                        a.scene_dirty = true;
                    }
                    None => a.flash("Select a plane or flat face to cut through", crate::app::ToastKind::Info),
                }),
            ));
            if app.section.is_some() {
                v.push(cmd(
                    "End section view",
                    "View",
                    "section",
                    None,
                    "cut clip off none",
                    Action::Run(|a| {
                        a.section = None;
                        a.scene_dirty = true;
                    }),
                ));
            }
            v.push(cmd(
                "Realistic rendering",
                "View",
                "sun",
                None,
                "lighting reflections render",
                Action::Run(|a| {
                    a.realistic = !a.realistic;
                    a.scene_dirty = true;
                }),
            ));
            v.push(cmd(
                "Printability view",
                "View",
                "printer",
                None,
                "overhang thin wall 3d print",
                Action::Run(|a| {
                    a.print_view = !a.print_view;
                    a.scene_dirty = true;
                }),
            ));
            for (label, ext) in [("Export STEP…", "step"), ("Export IGES…", "iges"), ("Export STL…", "stl")] {
                v.push(cmd(
                    label,
                    "File",
                    "export",
                    None,
                    "save download 3d print",
                    Action::Run(match ext {
                        "step" => |a| a.export("step"),
                        "iges" => |a| a.export("iges"),
                        _ => |a| a.export("stl"),
                    }),
                ));
            }
            v.push(cmd("Export OBJ…", "File", "export", None, "save download mesh", Action::Run(|a| a.export_obj())));
            v.push(cmd("Send to slicer", "File", "printer", None, "3d print orca", Action::Run(|a| a.send_to_slicer())));
            for (label, u) in [("Units: millimetres", "mm"), ("Units: centimetres", "cm"), ("Units: metres", "m"), ("Units: inches", "in"), ("Units: feet", "ft")] {
                v.push(cmd(
                    label,
                    "Units",
                    "ruler",
                    None,
                    "unit metric imperial mm cm in inch",
                    Action::Run(match u {
                        "mm" => |a| a.request_units(crate::expr::Units::Mm),
                        "cm" => |a| a.request_units(crate::expr::Units::Cm),
                        "m" => |a| a.request_units(crate::expr::Units::M),
                        "in" => |a| a.request_units(crate::expr::Units::In),
                        _ => |a| a.request_units(crate::expr::Units::Ft),
                    }),
                ));
            }
        }
        Tab::Cam(_) => {
            for k in OpKind::ALL {
                v.push(cmd(k.label(), "Operation", crate::cam_ui::kind_icon(k), None, "toolpath add", Action::CamOp(k)));
            }
            v.push(cmd("Simulate", "Manufacture", "camSim", None, "play stock check crash", Action::Run(crate::cam_ui::toggle_sim)));
            v.push(cmd("Export G-code…", "Manufacture", "camGcode", None, "post nc gcode save", Action::Run(crate::cam_ui::export_dialog)));
            v.push(cmd("Setup sheet…", "Manufacture", "camSheet", None, "print operator html", Action::Run(crate::cam_ui::sheet_dialog)));
            v.push(cmd("Tool library", "Manufacture", "camTools", None, "tools import fusion", Action::Run(|a| a.cam.library_open = true)));
        }
        Tab::Assembly(_) => {
            v.push(cmd("Standard parts…", "Assembly", "library", None, "screw bolt nut washer bearing pin dowel motor stepper nema extrusion iso din content library", Action::Run(crate::stdparts_ui::open_standard)));
            v.push(cmd("Insert from GrabCAD…", "Assembly", "import", None, "download step zip model library", Action::Run(crate::stdparts_ui::open_grabcad)));
        }
        Tab::Sim(_) => {
            v.push(cmd("Fixed support", "Simulation", "feaFixed", None, "fea constraint hold clamp", Action::Run(|a| crate::fea_ui::add_support(a, crate::fea::SupportKind::Fixed))));
            v.push(cmd("Roller support", "Simulation", "feaRoller", None, "fea constraint slide symmetry", Action::Run(|a| crate::fea_ui::add_support(a, crate::fea::SupportKind::Roller))));
            v.push(cmd("Force", "Simulation", "feaForce", None, "fea load", Action::Run(|a| crate::fea_ui::add_load(a, crate::fea::LoadKind::Force))));
            v.push(cmd("Pressure", "Simulation", "feaPressure", None, "fea load", Action::Run(|a| crate::fea_ui::add_load(a, crate::fea::LoadKind::Pressure))));
            v.push(cmd("Gravity", "Simulation", "feaGravity", None, "fea load weight acceleration", Action::Run(|a| crate::fea_ui::add_load(a, crate::fea::LoadKind::Gravity))));
            v.push(cmd("Animate", "Simulation", "camSim", None, "fea play mode vibration", Action::Run(crate::fea_ui::toggle_play)));
        }
        Tab::Pcb(_) => {
            for (label, icon, run) in crate::pcb_ui::commands_list() {
                v.push(cmd(label, "PCB", icon, None, "pcb circuit board schematic electronics", Action::Run(run)));
            }
        }
        _ => {}
    }
    if !matches!(app.tab, Tab::Drawing(_)) {
        v.push(cmd(
            "Fit view",
            "View",
            "fit",
            Some("F"),
            "zoom all",
            Action::Run(|a| match a.tab {
                Tab::Cam(_) => crate::cam_ui::fit(a),
                Tab::Sim(_) => crate::fea_ui::fit(a),
                Tab::Pcb(_) => crate::pcb_ui::fit(a),
                _ => a.fit_view(),
            }),
        ));
    }
    if matches!(app.tab, Tab::Studio(_) | Tab::Assembly(_)) {
        v.push(cmd("Export web viewer (HTML)…", "File", "export", None, "save share browser html", Action::Run(|a| a.export_web())));
    }
    v.push(cmd("Screenshot (PNG)…", "File", "export", None, "image picture save", Action::Run(|a| a.session.screenshot = true)));
    let theme = if app.dark { "Light mode" } else { "Dark mode" };
    v.push(cmd(
        theme,
        "Theme",
        if app.dark { "sun" } else { "moon" },
        None,
        "theme dark light colours appearance",
        Action::Run(|a| a.session.toggle_dark = true),
    ));
    v.push(cmd("Keyboard shortcuts", "Help", "keyboard", Some("F1"), "keys help hotkeys list", Action::Run(|a| a.session.show_shortcuts = true)));
    v.push(cmd("New document", "File", "file", Some("Ctrl+N"), "new part blank start", Action::Run(|a| a.guard(crate::session::Pending::New))));
    v.push(cmd("Version control (git)", "File", "branch", None, "git github history commit push pull track changes versions repository", Action::Run(crate::vcs_ui::toggle)));
    v.push(cmd("New project…", "File", "folder", None, "project folder create start git", Action::Run(crate::vcs_ui::new_project)));
    v.push(cmd("Export project (ZIP)…", "File", "export", None, "project zip archive share backup send", Action::Run(crate::vcs_ui::export_project)));
    v.push(cmd("Import project (ZIP)…", "File", "download", None, "project zip archive unpack open", Action::Run(crate::vcs_ui::import_project)));
    v.push(cmd("Design with Claude", "Claude", "chat", Some("Ctrl+L"), "ai chat natural language assistant agent", Action::Run(crate::claude_ui::toggle)));
    v.push(cmd("Undo", "Edit", "undo", Some("Ctrl+Z"), "", Action::Run(|a| a.undo())));
    v.push(cmd("Redo", "Edit", "redo", Some("Ctrl+Y"), "", Action::Run(|a| a.redo())));
    v.push(cmd("Save", "File", "save", Some("Ctrl+S"), "", Action::Run(|a| a.save(false))));
    v.push(cmd("Save as…", "File", "save", Some("Ctrl+Shift+S"), "copy", Action::Run(|a| a.save(true))));
    v.push(cmd("Open…", "File", "folder", Some("Ctrl+O"), "load document", Action::Run(|a| a.open_dialog())));
    for (label, icon, what) in [
        ("New Part Studio", "partStudio", "new-studio"),
        ("New Assembly", "assembly", "new-assembly"),
        ("New Drawing", "drawing", "new-drawing"),
        ("New Manufacture (CAM)", "cam", "new-cam"),
        ("New Simulation (FEA)", "simulation", "new-sim"),
        ("New PCB", "pcb", "new-pcb"),
    ] {
        v.push(cmd(
            label,
            "Tab",
            icon,
            None,
            "add create",
            Action::Run(match what {
                "new-studio" => |a| a.tab_action("new-studio", a.tab),
                "new-assembly" => |a| a.tab_action("new-assembly", a.tab),
                "new-drawing" => |a| a.tab_action("new-drawing", a.tab),
                "new-sim" => |a| a.tab_action("new-sim", a.tab),
                "new-pcb" => |a| a.tab_action("new-pcb", a.tab),
                _ => |a| a.tab_action("new-cam", a.tab),
            }),
        ));
    }
    // go to another tab
    let tabs = app.ws.studios.iter().enumerate().map(|(i, s)| (Tab::Studio(i), s.name.clone(), "partStudio"));
    let tabs = tabs.chain(app.ws.assemblies.iter().enumerate().map(|(i, s)| (Tab::Assembly(i), s.name.clone(), "assembly")));
    let tabs = tabs.chain(app.ws.drawings.iter().enumerate().map(|(i, s)| (Tab::Drawing(i), s.name.clone(), "drawing")));
    let tabs = tabs.chain(app.ws.cams.iter().enumerate().map(|(i, s)| (Tab::Cam(i), s.name.clone(), "cam")));
    let tabs = tabs.chain(app.ws.sims.iter().enumerate().map(|(i, s)| (Tab::Sim(i), s.name.clone(), "simulation")));
    let tabs = tabs.chain(app.ws.pcbs.iter().enumerate().map(|(i, s)| (Tab::Pcb(i), s.name.clone(), "pcb")));
    for (t, name, icon) in tabs {
        if t != app.tab {
            v.push(cmd(format!("Go to {name}"), "Tab", icon, None, "switch open", Action::Tab(t)));
        }
    }
    v
}

fn set_view(a: &mut App, v: crate::camera::StdView) {
    a.camera.set_view(v);
    a.scene_dirty = true;
}

fn set_section(a: &mut App, plane: u8) {
    a.section = Some(a.section_for(plane, false));
    a.scene_dirty = true;
}

/// How well `q` matches: words that start a word of the label score best, then words inside it, then
/// letters in order. None = no match.
fn score(c: &Cmd, q: &str) -> Option<i32> {
    let label = c.label.to_lowercase();
    let hay = format!("{label} {} {}", c.group.to_lowercase(), c.words);
    let mut total = 0;
    for w in q.split_whitespace() {
        let starts = label.split(|ch: char| !ch.is_alphanumeric()).any(|x| x.starts_with(w));
        // the whole word typed beats a longer word starting with it
        if label.split(|ch: char| !ch.is_alphanumeric()).any(|x| x == w) {
            total += 5;
        }
        total += if label.starts_with(w) {
            100
        } else if starts {
            70
        } else if label.contains(w) {
            45
        } else if hay.contains(w) {
            25
        } else {
            // letters in order ("dim" in "dimension", "xtr" in "extrude")
            let mut it = label.chars();
            if w.chars().all(|ch| it.any(|x| x == ch)) {
                10
            } else {
                return None;
            }
        };
    }
    // among equals the command list's order decides (common commands come first there)
    Some(total)
}

fn ranked(app: &App) -> Vec<Cmd> {
    let all = commands(app);
    let q = app.palette.query.trim().to_lowercase();
    if q.is_empty() {
        let mut v: Vec<Cmd> = app.palette.recent.iter().filter_map(|r| all.iter().find(|c| &c.label == r).cloned()).collect();
        v.extend(all.into_iter().filter(|c| !app.palette.recent.contains(&c.label)));
        return v;
    }
    let mut v: Vec<(i32, Cmd)> = all.into_iter().filter_map(|c| score(&c, &q).map(|s| (s, c))).collect();
    // stable: equal scores keep the list's order
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|x| x.1).collect()
}

pub fn open(app: &mut App) {
    app.palette.open = true;
    app.palette.query.clear();
    app.palette.sel = 0;
    app.palette.focus = true;
}

/// Open it from the keyboard: S (where S isn't a tool's key), / or Ctrl+K.
pub fn shortcut(app: &mut App, ctx: &egui::Context) {
    if app.palette.open || ctx.egui_wants_keyboard_input() {
        return;
    }
    let (ctrl, shift) = crate::app::key_mods(ctx);
    let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
    let s_free = app.sketch.is_none() && !matches!(app.tab, Tab::Drawing(_));
    if (ctrl && pressed(Key::K)) || (!ctrl && !shift && (pressed(Key::Slash) || (s_free && pressed(Key::S)))) {
        open(app);
    }
}

pub fn window(app: &mut App, ctx: &egui::Context) {
    if !app.palette.open {
        return;
    }
    let list = ranked(app);
    let shown = list.len();
    let moved = ctx.input(|i| i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowUp));
    if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
        app.palette.sel = (app.palette.sel + 1).min(shown.saturating_sub(1));
    }
    if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
        app.palette.sel = app.palette.sel.saturating_sub(1);
    }
    let mut run = None;
    let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
    let width = 420.0f32.min(ctx.content_rect().width() - 24.0);
    let top = ctx.content_rect().center_top() + egui::vec2(-width / 2.0, 70.0);
    let area = egui::Area::new(egui::Id::new("command-search")).order(egui::Order::Foreground).fixed_pos(top).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(width);
            let r = ui.add(
                egui::TextEdit::singleline(&mut app.palette.query)
                    .hint_text(RichText::new("Search commands…").color(theme::muted()))
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(8.0, 6.0)),
            );
            if app.palette.focus {
                r.request_focus();
                app.palette.focus = false;
            }
            if r.changed() {
                app.palette.sel = 0;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                run = list.get(app.palette.sel).cloned();
            }
            ui.add_space(4.0);
            if list.is_empty() {
                ui.label(RichText::new("No command matches").color(theme::muted()).size(12.5));
            }
            // about 12 rows, scrolling for more
            let rows = egui::ScrollArea::vertical().max_height(12.0 * 26.0).auto_shrink([false, true]).show(ui, |ui| {
            let mut run = None;
            for (k, c) in list.iter().enumerate() {
                let sel = k == app.palette.sel;
                let resp = ui
                    .horizontal(|ui| {
                        ui.add(crate::icons::image(&c.icon, 16.0).tint(theme::text()));
                        let b = ui.add(egui::Button::selectable(sel, RichText::new(&c.label).size(13.0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(k) = &c.keys {
                                ui.label(RichText::new(k).color(theme::muted()).size(11.5).monospace());
                            }
                            ui.label(RichText::new(c.group).color(theme::muted()).size(11.5));
                        });
                        b
                    })
                    .inner;
                if resp.clicked() {
                    run = Some(c.clone());
                }
                if sel && moved {
                    resp.scroll_to_me(None);
                }
                if resp.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    app.palette.sel = k;
                }
            }
            run
            });
            if rows.inner.is_some() {
                run = rows.inner;
            }
            if app.palette.query.is_empty() && !app.palette.recent.is_empty() {
                ui.label(RichText::new("Recent first · ↑↓ to choose · Enter to run · Esc to close").color(theme::muted()).size(11.0));
            } else {
                ui.label(RichText::new("↑↓ to choose · Enter to run · Esc to close").color(theme::muted()).size(11.0));
            }
        });
    });
    // a click outside closes it
    if run.is_none() && ctx.input(|i| i.pointer.any_click()) && !area.response.contains_pointer() {
        close = true;
    }
    if let Some(c) = run {
        app.palette.open = false;
        app.palette.recent.retain(|r| r != &c.label);
        app.palette.recent.insert(0, c.label.clone());
        app.palette.recent.truncate(8);
        match c.action {
            Action::Feature(k) => {
                if app.kernel_ready() {
                    app.start_feature(k);
                }
            }
            Action::SketchTool(t) => crate::sketcher::set_tool(app, t),
            Action::Constrain(k) => crate::sketcher::constrain(app, k),
            Action::CamOp(k) => crate::cam_ui::add_op(app, k),
            Action::Tab(t) => app.switch_tab(t),
            Action::Run(f) => f(app),
        }
        app.scene_dirty = true;
    } else if close {
        app.palette.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_prefers_word_starts() {
        let c = |label: &str| cmd(label, "Feature", "x", None, "", Action::Feature("x"));
        let q = "ex";
        let ext = score(&c("Extrude"), q).unwrap();
        let next = score(&c("Make next"), q).unwrap();
        assert!(ext > next);
        assert!(score(&c("Extrude"), "xtr").is_some());
        assert!(score(&c("Fillet"), "zz").is_none());
        // every word has to match
        assert!(score(&c("Export G-code…"), "export g").is_some() && score(&c("Export G-code…"), "export q").is_none());
        // found by its group and extra words too
        assert!(score(&cmd("Simulate", "Manufacture", "x", None, "crash", Action::Feature("x")), "crash").is_some());
        // a name match beats a group match
        assert!(score(&c("Section view: Top plane"), "view") > score(&cmd("Sketch", "View", "x", None, "", Action::Feature("x")), "view"));
    }

    #[test]
    fn fillet_before_fill() {
        // equal prefix matches keep the command list's order, which has Fillet before Fill
        let fil: Vec<&str> = crate::app::FEATURE_GROUPS.iter().flat_map(|g| g.iter().copied()).filter(|k| k.starts_with("fill")).collect();
        assert_eq!(fil, ["fillet", "fill"]);
        let c = |label: &str| cmd(label, "Feature", "x", None, "", Action::Feature("x"));
        assert_eq!(score(&c("Fillet"), "fil"), score(&c("Fill"), "fil"));
        // the whole word still wins when typed
        assert!(score(&c("Fill"), "fill") > score(&c("Fillet"), "fill"));
    }
}
