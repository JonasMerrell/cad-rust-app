//! Editor for custom feature scripts (the document's "Feature Studio").

use egui::{RichText, Ui};

use crate::app::App;
use crate::doc::Op;
use crate::script::{CustomDef, ParamDef, EXAMPLE, PARAM_KINDS};
use crate::theme;

pub struct ScriptEditor {
    /// Definition being edited (index into the document's list), None for a new one.
    pub index: Option<usize>,
    pub def: CustomDef,
}

pub fn open(app: &mut App, index: Option<usize>) {
    let def = match index.and_then(|i| app.doc.custom_features.get(i)) {
        Some(d) => d.clone(),
        None => CustomDef {
            name: unique_name(app, "Bosses"),
            params: vec![
                ParamDef { name: "diameter".into(), kind: "length".into(), default: "8".into() },
                ParamDef { name: "height".into(), kind: "length".into(), default: "10".into() },
                ParamDef { name: "fillet".into(), kind: "length".into(), default: "1".into() },
            ],
            selection: "points".into(),
            source: EXAMPLE.into(),
        },
    };
    app.script_editor = Some(ScriptEditor { index, def });
}

fn unique_name(app: &App, base: &str) -> String {
    let mut n = 1;
    loop {
        let name = if n == 1 { base.to_string() } else { format!("{base} {n}") };
        if !app.doc.custom_features.iter().any(|d| d.name == name) {
            return name;
        }
        n += 1;
    }
}

/// Start a Custom feature of definition `i`.
pub fn start(app: &mut App, i: usize) {
    let Some(def) = app.doc.custom_features.get(i).cloned() else { return };
    if app.sketch.is_some() {
        return;
    }
    if app.dialog.is_some() {
        app.dialog_ok();
    }
    let mut op = Op::Custom { name: def.name.clone(), values: def.params.iter().map(|p| (p.name.clone(), p.default.clone())).collect(), refs: vec![], select: def.selection.clone(), op: crate::doc::BoolOp::Add };
    crate::dialog::seed_from_picks(&mut op, &app.picks, &app.doc);
    if app.result.bodies.is_empty() {
        if let Op::Custom { op, .. } = &mut op {
            *op = crate::doc::BoolOp::New;
        }
    }
    let before = app.doc.clone();
    app.checkpoint();
    let n = app.doc.features.iter().filter(|f| matches!(&f.op, Op::Custom { name, .. } if *name == def.name)).count() + 1;
    let id = app.doc.insert(format!("{} {n}", def.name), op);
    app.picks.clear();
    app.dialog = Some(crate::app::Dialog { feature: id, is_new: true, before, active_field: 0 });
    app.changed();
}

pub fn window(app: &mut App, ctx: &egui::Context) {
    let Some(ed) = &mut app.script_editor else { return };
    let mut shown = true;
    let mut save = false;
    let mut delete = false;
    let mut switch: Option<Option<usize>> = None;
    let defs: Vec<String> = app.doc.custom_features.iter().map(|d| d.name.clone()).collect();
    let cur = ed.index;
    egui::Window::new("Custom features").open(&mut shown).default_size([760.0, 560.0]).resizable(true).show(ctx, |ui| {
        ui.horizontal_top(|ui| {
            // list of definitions
            ui.vertical(|ui| {
                ui.set_width(150.0);
                for (i, n) in defs.iter().enumerate() {
                    if ui.selectable_label(cur == Some(i), n).clicked() {
                        switch = Some(Some(i));
                    }
                }
                if ui.selectable_label(cur.is_none(), "+ New feature").clicked() && cur.is_some() {
                    switch = Some(None);
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                let d = &mut ed.def;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Name").color(theme::muted()));
                    ui.text_edit_singleline(&mut d.name);
                    ui.label(RichText::new("Selection").color(theme::muted()));
                    let label = match d.selection.as_str() {
                        "points" => "Points",
                        "faces" => "Faces",
                        _ => "None",
                    };
                    egui::ComboBox::from_id_salt("script-sel").selected_text(label).show_ui(ui, |ui| {
                        for (v, l) in [("", "None"), ("points", "Points"), ("faces", "Faces")] {
                            ui.selectable_value(&mut d.selection, v.to_string(), l);
                        }
                    });
                });
                ui.label(RichText::new("PARAMETERS").color(theme::muted()).size(11.0).strong());
                let mut remove = None;
                egui::Grid::new("script-params").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                    for (i, p) in d.params.iter_mut().enumerate() {
                        ui.add_sized([120.0, 20.0], egui::TextEdit::singleline(&mut p.name).hint_text("name"));
                        let kl = PARAM_KINDS.iter().find(|k| k.0 == p.kind).map(|k| k.1).unwrap_or("Number");
                        egui::ComboBox::from_id_salt(("pk", i)).selected_text(kl).width(100.0).show_ui(ui, |ui| {
                            for (k, l) in PARAM_KINDS {
                                ui.selectable_value(&mut p.kind, k.to_string(), *l);
                            }
                        });
                        ui.add_sized([90.0, 20.0], egui::TextEdit::singleline(&mut p.default).hint_text("default"));
                        if ui.add(egui::Button::image(crate::icons::image("close", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    d.params.remove(i);
                }
                if ui.button("+ Parameter").clicked() {
                    d.params.push(ParamDef { name: format!("p{}", d.params.len() + 1), kind: "length".into(), default: "10".into() });
                }
                ui.add_space(4.0);
                ui.label(RichText::new("SCRIPT (Rhai) · parameters are variables in mm / degrees; `points` and `normals` hold the selection").color(theme::muted()).size(11.0));
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut d.source).code_editor().desired_rows(18).desired_width(f32::INFINITY));
                });
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Save").strong()).on_hover_text("Save the definition; features using it rebuild").clicked() {
                        save = true;
                    }
                    if cur.is_some() && ui.button(RichText::new("Delete").color(theme::error_soft())).clicked() {
                        delete = true;
                    }
                });
            });
        });
    });
    if save {
        let (index, def) = app.script_editor.as_ref().map(|e| (e.index, e.def.clone())).unwrap();
        if def.name.trim().is_empty() {
            app.flash("Give the feature a name", crate::app::ToastKind::Info);
            return;
        }
        app.checkpoint();
        match index {
            Some(i) => {
                let old = app.doc.custom_features[i].name.clone();
                app.doc.custom_features[i] = def.clone();
                // features follow a rename
                for f in &mut app.doc.features {
                    if let Op::Custom { name, select, .. } = &mut f.op {
                        if *name == old {
                            *name = def.name.clone();
                            *select = def.selection.clone();
                        }
                    }
                }
            }
            None => {
                app.doc.custom_features.push(def);
                let n = app.doc.custom_features.len() - 1;
                if let Some(e) = &mut app.script_editor {
                    e.index = Some(n);
                }
            }
        }
        app.changed();
        app.flash_info("Saved · it is on the toolbar", Some(std::time::Duration::from_secs(2)));
    }
    if delete {
        if let Some(i) = app.script_editor.as_ref().and_then(|e| e.index) {
            app.checkpoint();
            app.doc.custom_features.remove(i);
            app.changed();
            open(app, None);
        }
    }
    if let Some(s) = switch {
        open(app, s);
    }
    if !shown {
        app.script_editor = None;
    }
}

pub fn toolbar(app: &mut App, ui: &mut Ui, ready: bool) {
    App::group(ui, |ui| {
        if app.tool_button(ui, "custom", "Custom features", "Write your own features as scripts", false, app.script_editor.is_some(), true).clicked() {
            if app.script_editor.is_some() {
                app.script_editor = None;
            } else {
                open(app, if app.doc.custom_features.is_empty() { None } else { Some(0) });
            }
        }
        let names: Vec<String> = app.doc.custom_features.iter().map(|d| d.name.clone()).collect();
        for (i, n) in names.iter().enumerate() {
            if app.tool_button(ui, "wand", n, &format!("{n} (custom feature)"), true, false, ready).clicked() {
                start(app, i);
            }
        }
    });
}
