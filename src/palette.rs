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
            v.push(cmd("Import a model or drawing…", "File", "import", None, "step iges stl obj dxf svg", Action::Run(|a| a.import_step())));
            for (label, view, key) in [("Iso view", "iso", "0"), ("Front view", "front", "1"), ("Top view", "top", "2"), ("Right view", "right", "3")] {
                v.push(cmd(
                    label,
                    "View",
                    "isoView",
                    Some(key),
                    "camera",
                    Action::Run(match view {
                        "front" => |a| set_view(a, crate::camera::StdView::Front),
                        "top" => |a| set_view(a, crate::camera::StdView::Top),
                        "right" => |a| set_view(a, crate::camera::StdView::Right),
                        _ => |a| set_view(a, crate::camera::StdView::Iso),
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
        Tab::Sim(_) => {
            v.push(cmd("Fixed support", "Simulation", "feaFixed", None, "fea constraint hold clamp", Action::Run(|a| crate::fea_ui::add_support(a, crate::fea::SupportKind::Fixed))));
            v.push(cmd("Roller support", "Simulation", "feaRoller", None, "fea constraint slide symmetry", Action::Run(|a| crate::fea_ui::add_support(a, crate::fea::SupportKind::Roller))));
            v.push(cmd("Force", "Simulation", "feaForce", None, "fea load", Action::Run(|a| crate::fea_ui::add_load(a, crate::fea::LoadKind::Force))));
            v.push(cmd("Pressure", "Simulation", "feaPressure", None, "fea load", Action::Run(|a| crate::fea_ui::add_load(a, crate::fea::LoadKind::Pressure))));
            v.push(cmd("Gravity", "Simulation", "feaGravity", None, "fea load weight acceleration", Action::Run(|a| crate::fea_ui::add_load(a, crate::fea::LoadKind::Gravity))));
            v.push(cmd("Animate", "Simulation", "camSim", None, "fea play mode vibration", Action::Run(crate::fea_ui::toggle_play)));
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
                _ => a.fit_view(),
            }),
        ));
    }
    v.push(cmd("Undo", "Edit", "undo", Some("Ctrl+Z"), "", Action::Run(|a| a.undo())));
    v.push(cmd("Redo", "Edit", "redo", Some("Ctrl+Y"), "", Action::Run(|a| a.redo())));
    v.push(cmd("Save", "File", "download", Some("Ctrl+S"), "", Action::Run(|a| a.save(false))));
    v.push(cmd("Save as…", "File", "download", Some("Ctrl+Shift+S"), "", Action::Run(|a| a.save(true))));
    v.push(cmd("Open…", "File", "file", Some("Ctrl+O"), "load document", Action::Run(|a| a.open_dialog())));
    for (label, icon, what) in [
        ("New Part Studio", "partStudio", "new-studio"),
        ("New Assembly", "assembly", "new-assembly"),
        ("New Drawing", "drawing", "new-drawing"),
        ("New Manufacture (CAM)", "cam", "new-cam"),
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

/// How well `q` matches: words that start a word of the label score best, then words inside it, then
/// letters in order. None = no match.
fn score(c: &Cmd, q: &str) -> Option<i32> {
    let label = c.label.to_lowercase();
    let hay = format!("{label} {} {}", c.group.to_lowercase(), c.words);
    let mut total = 0;
    for w in q.split_whitespace() {
        let starts = label.split(|ch: char| !ch.is_alphanumeric()).any(|x| x.starts_with(w));
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
    // shorter labels first among equals
    Some(total * 100 - label.len() as i32)
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
    let shown = list.len().min(12);
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
            for (k, c) in list.iter().take(shown).enumerate() {
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
                if resp.hovered() {
                    app.palette.sel = k;
                }
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
    }
}
