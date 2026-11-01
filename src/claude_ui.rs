//! The Claude chat panel: type a design request, Claude edits the document.

use egui::{Align, Frame, Key, Layout, Margin, RichText, ScrollArea, Ui};

use crate::app::App;
use crate::claude::{Auth, EFFORTS, FromWorker, MODELS, Settings, ToWorker, Worker};
use crate::theme;

enum Entry {
    User(String),
    Assistant(String),
    Thinking(String),
    Tool { name: String, detail: String, result: Option<(bool, String)>, image: Option<egui::TextureHandle> },
    Notice { text: String, error: bool },
}

pub struct ClaudeUi {
    pub open: bool,
    settings: Settings,
    settings_open: bool,
    key_input: String,
    input: String,
    entries: Vec<Entry>,
    worker: Option<Worker>,
    busy: bool,
    /// The document before Claude's last turn, to undo the whole turn.
    before: Option<(String, usize)>,
    changed: bool,
    login: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// A "Check" running off the UI thread.
    check: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    login_status: Option<Result<String, String>>,
    width: f32,
    /// Where the current model request's streamed text starts: entries, and the length of the
    /// last entry's text when the request continues it.
    attempt: (usize, Option<usize>),
    /// `App::content_hash` when Claude's last request ended, to notice later edits before
    /// "Undo last request" throws them away.
    after_hash: Option<u64>,
    confirm_revert: bool,
}

impl Default for ClaudeUi {
    fn default() -> Self {
        let settings = crate::claude::load_settings();
        ClaudeUi {
            open: false,
            settings,
            settings_open: false,
            key_input: String::new(),
            input: String::new(),
            entries: vec![],
            worker: None,
            busy: false,
            before: None,
            changed: false,
            login: None,
            check: None,
            login_status: None,
            width: 360.0,
            attempt: (0, None),
            after_hash: None,
            confirm_revert: false,
        }
    }
}

impl ClaudeUi {
    /// Claude is working on the document: the editor is locked meanwhile.
    pub fn busy(&self) -> bool {
        self.busy
    }

    fn configured(&self) -> bool {
        match self.settings.auth {
            Auth::ApiKey => self.settings.key().is_some(),
            Auth::Console | Auth::ClaudeCode => true,
        }
    }
}

pub fn toggle(app: &mut App) {
    app.claude.open = !app.claude.open;
}

/// Handle what the worker sent, then draw the panel (when open).
pub fn panel(app: &mut App, ui: &mut Ui) {
    poll(app, ui.ctx());
    if !app.claude.open || app.phone {
        lock_editor(app, ui.ctx(), ui.ctx().content_rect());
        return;
    }
    let frame = Frame::new().fill(theme::panel()).inner_margin(Margin { left: 12, right: 10, top: 8, bottom: 8 });
    let r = egui::Panel::right("claude").frame(frame).resizable(false).exact_size(app.claude.width).show(ui, |ui| {
        ui.set_max_width(ui.available_width());
        header(app, ui);
        ui.add_space(4.0);
        if app.claude.settings_open || !app.claude.configured() {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| settings(app, ui));
        } else {
            chat(app, ui);
        }
    });
    // drag the panel's left edge to resize it
    let rect = r.response.rect;
    let edge = egui::Rect::from_min_max(egui::pos2(rect.left() - 3.0, rect.top()), egui::pos2(rect.left() + 3.0, rect.bottom()));
    let h = ui.interact(edge, egui::Id::new("claude-resize"), egui::Sense::drag());
    if h.hovered() || h.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if h.dragged() {
        app.claude.width = (app.claude.width - h.drag_delta().x).clamp(260.0, 700.0);
    }
    ui.painter().vline(rect.left(), rect.y_range(), egui::Stroke::new(1.0, theme::border()));
    let screen = ui.ctx().content_rect();
    lock_editor(app, ui.ctx(), egui::Rect::from_min_max(screen.min, egui::pos2(rect.left() - 4.0, screen.max.y)));
}

/// While Claude works on its copy of the document, edits in the editor would be overwritten by
/// its next change: cover the editor (`area`) so it can't be edited, with a Stop button.
/// Keyboard shortcuts are held back by the caller (`ClaudeUi::busy`).
fn lock_editor(app: &mut App, ctx: &egui::Context, area: egui::Rect) {
    if !app.claude.busy || area.width() <= 0.0 {
        return;
    }
    let stop = egui::Area::new(egui::Id::new("claude-lock")).order(egui::Order::Foreground).fixed_pos(area.min).show(ctx, |ui| {
        let (r, _) = ui.allocate_exact_size(area.size(), egui::Sense::click_and_drag());
        ui.painter().rect_filled(r, 0.0, egui::Color32::from_black_alpha(40));
        let card = egui::Rect::from_center_size(egui::pos2(r.center().x, r.top() + 60.0), egui::vec2(300.0, 64.0));
        let mut stop = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
            Frame::new().fill(theme::panel()).corner_radius(8).inner_margin(Margin::same(10)).show(ui, |ui| {
                ui.label(RichText::new("Claude is changing the document").size(12.5).color(theme::text()));
                stop = ui.button("Stop to edit it yourself").clicked();
            });
        });
        stop
    });
    if stop.inner {
        if let Some(w) = &app.claude.worker {
            w.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn header(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.add(crate::icons::image("chat", 17.0).tint(theme::accent()));
        ui.label(RichText::new("Claude").family(theme::medium()).size(14.0).color(theme::text()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(egui::Button::image(crate::icons::image("close", 15.0).tint(theme::text()))).on_hover_text("Close").clicked() {
                app.claude.open = false;
            }
            let c = &mut app.claude;
            if c.configured()
                && ui.add(egui::Button::image(crate::icons::image("properties", 15.0).tint(theme::text())).selected(c.settings_open)).on_hover_text("Account and model").clicked()
            {
                c.settings_open = !c.settings_open;
            }
            if !c.entries.is_empty() && !c.busy && ui.add(egui::Button::image(crate::icons::image("plus", 15.0).tint(theme::text()))).on_hover_text("New chat").clicked() {
                c.entries.clear();
                c.before = None;
                if let Some(w) = &c.worker {
                    let _ = w.tx.send(ToWorker::Reset);
                }
            }
        });
    });
}

/// Run a check that starts a program (and may wait on the network) without freezing the window.
fn off_thread(ctx: &egui::Context, f: fn() -> Result<String, String>) -> std::sync::mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    rx
}

fn settings(app: &mut App, ui: &mut Ui) {
    let c = &mut app.claude;
    let muted = |ui: &mut Ui, t: &str| {
        ui.label(RichText::new(t).size(11.5).color(theme::muted()));
    };
    ui.label(
        RichText::new("Design in plain language: describe a part or a change, and Claude builds it in this document with the same tools as the MCP server.")
            .size(12.0)
            .color(theme::text()),
    );
    ui.add_space(10.0);
    ui.label(RichText::new("CONNECT").family(theme::medium()).color(theme::muted()).size(10.5));
    ui.add_space(4.0);
    let mut auth = c.settings.auth;
    ui.radio_value(&mut auth, Auth::ClaudeCode, "Claude Code (your Claude login)");
    ui.radio_value(&mut auth, Auth::ApiKey, "Anthropic API key");
    ui.radio_value(&mut auth, Auth::Console, "Claude Console account (ant CLI sign-in)");
    if auth != c.settings.auth {
        c.settings.auth = auth;
        let _ = crate::claude::save_settings(&c.settings);
    }
    ui.add_space(6.0);
    match c.settings.auth {
        Auth::ApiKey => {
            if c.settings.key_from_env() {
                muted(ui, "Using ANTHROPIC_API_KEY from the environment.");
            } else if !c.settings.api_key.is_empty() {
                let k = &c.settings.api_key;
                let tail: String = k.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                ui.horizontal(|ui| {
                    muted(ui, &format!("Key saved (…{tail})"));
                    if ui.small_button("Remove").clicked() {
                        c.settings.api_key.clear();
                        let _ = crate::claude::save_settings(&c.settings);
                    }
                });
            }
            ui.horizontal(|ui| {
                theme::field_style(ui);
                let w = ui.available_width() - 60.0;
                let r = ui.add(egui::TextEdit::singleline(&mut c.key_input).password(true).hint_text("sk-ant-…").desired_width(w));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.button("Save").clicked() || enter) && !c.key_input.trim().is_empty() {
                    c.settings.api_key = c.key_input.trim().to_string();
                    c.key_input.clear();
                    match crate::claude::save_settings(&c.settings) {
                        Ok(p) => c.login_status = Some(Ok(format!("Saved to {}", p.display()))),
                        Err(e) => c.login_status = Some(Err(format!("Could not save the key: {e}"))),
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                muted(ui, "Create one at");
                ui.hyperlink_to(RichText::new("platform.claude.com").size(11.5), "https://platform.claude.com/settings/keys");
                muted(ui, ". It is stored in your config folder, readable only by you. Usage is billed to that API account.");
            });
        }
        Auth::Console => {
            muted(ui, "Signs in with the Anthropic CLI (ant) in your browser; usage is billed to the Console organization you pick.");
            ui.horizontal(|ui| {
                let waiting = c.login.is_some();
                if ui.add_enabled(!waiting, egui::Button::new(if waiting { "Waiting for the browser…" } else { "Sign in" })).clicked() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::claude::console_login());
                        ctx.request_repaint();
                    });
                    c.login = Some(rx);
                }
                if ui.add_enabled(!waiting && c.check.is_none(), egui::Button::new("Check")).clicked() {
                    c.check = Some(off_thread(ui.ctx(), crate::claude::console_status));
                }
            });
        }
        Auth::ClaudeCode => {
            muted(
                ui,
                "Runs the Claude Code CLI on this computer with its own login, so requests use your Claude plan (or whatever Claude Code is signed in with). Install Claude Code and run claude once to sign in.",
            );
            if ui.add_enabled(c.check.is_none(), egui::Button::new("Check")).clicked() {
                c.check = Some(off_thread(ui.ctx(), crate::claude::claude_code_version));
            }
        }
    }
    if let Some(rx) = &c.login {
        if let Ok(r) = rx.try_recv() {
            c.login_status = Some(r);
            c.login = None;
        }
    }
    if let Some(rx) = &c.check {
        match rx.try_recv() {
            Ok(r) => {
                c.login_status = Some(r);
                c.check = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => c.check = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    muted(ui, "Checking…");
                });
            }
        }
    }
    if let Some(s) = &c.login_status {
        let (t, col) = match s {
            Ok(t) => (t.as_str(), theme::muted()),
            Err(t) => (t.as_str(), theme::error()),
        };
        ui.label(RichText::new(t).size(11.5).color(col));
    }

    ui.add_space(12.0);
    ui.label(RichText::new("MODEL").family(theme::medium()).color(theme::muted()).size(10.5));
    ui.add_space(4.0);
    let mut changed = false;
    ui.horizontal(|ui| {
        theme::field_style(ui);
        let label = MODELS.iter().find(|m| m.0 == c.settings.model).map(|m| m.1).unwrap_or(c.settings.model.as_str()).to_string();
        egui::ComboBox::from_id_salt("claude-model").selected_text(label).width(170.0).show_ui(ui, |ui| {
            for (id, name) in MODELS {
                changed |= ui.selectable_value(&mut c.settings.model, id.to_string(), *name).changed();
            }
        });
        egui::ComboBox::from_id_salt("claude-effort").selected_text(format!("Effort: {}", c.settings.effort)).width(110.0).show_ui(ui, |ui| {
            for e in EFFORTS {
                changed |= ui.selectable_value(&mut c.settings.effort, e.to_string(), *e).changed();
            }
        });
    });
    muted(ui, "Higher effort thinks longer and costs more; high suits most modelling.");
    if changed {
        let _ = crate::claude::save_settings(&c.settings);
    }
    if c.configured() {
        ui.add_space(10.0);
        if ui.button("Done").clicked() {
            c.settings_open = false;
        }
    }
}

fn chat(app: &mut App, ui: &mut Ui) {
    let input_h = 96.0;
    let list_h = (ui.available_height() - input_h).max(80.0);
    ScrollArea::vertical().id_salt("claude-chat").auto_shrink([false, false]).max_height(list_h).stick_to_bottom(true).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if app.claude.entries.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Describe what to build or change, for example:").size(12.0).color(theme::muted()));
            for ex in [
                "A 60 × 40 × 8 mm mounting plate with four M5 clearance holes 6 mm from the edges and 2 mm fillets on the corners",
                "Make the selected edges 1 mm chamfers",
                "Add a variable for the wall thickness and shell the part with it",
                "Set up a Manufacture tab for this part in 6061 aluminium with a 6 mm flat end mill",
            ] {
                if ui.add(egui::Button::new(RichText::new(ex).size(12.0)).wrap().fill(theme::panel_2())).clicked() {
                    app.claude.input = ex.to_string();
                }
            }
        }
        for e in &app.claude.entries {
            entry(ui, e);
        }
        if app.claude.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Working…").size(12.0).color(theme::muted()));
            });
        }
    });
    ui.separator();
    let c = &mut app.claude;
    theme::field_style(ui);
    let r = ui.add(
        egui::TextEdit::multiline(&mut c.input)
            .hint_text("Ask Claude to design or change something… (Enter sends, Shift+Enter for a new line)")
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    let enter = r.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
    if enter {
        // drop the newline the key press added
        while c.input.ends_with('\n') {
            c.input.pop();
        }
    }
    let (mut go, mut revert) = (false, false);
    ui.horizontal(|ui| {
        if c.before.is_some() && c.changed && !c.busy {
            revert = ui.small_button("Undo last request").on_hover_text("Put the document back the way it was before Claude's last request").clicked();
        } else {
            let model = MODELS.iter().find(|m| m.0 == c.settings.model).map(|m| m.1).unwrap_or("");
            ui.label(RichText::new(format!("{model} · {}", c.settings.effort)).size(11.0).color(theme::muted()));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if c.busy {
                if ui.button("Stop").clicked() {
                    if let Some(w) = &c.worker {
                        w.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            } else {
                go = (ui.add_enabled(!c.input.trim().is_empty(), egui::Button::new("Send")).clicked() || enter) && !c.input.trim().is_empty();
            }
        });
    });
    if go {
        send(app, ui.ctx());
    }
    if revert {
        // edits made since would go too: say so, and undo on the second click
        app.sync_ws();
        let edited = app.claude.after_hash.is_some_and(|h| h != app.content_hash());
        if edited && !app.claude.confirm_revert {
            app.claude.confirm_revert = true;
            app.claude.entries.push(Entry::Notice {
                text: "The document has changed since Claude's request. Undoing the request also discards those changes; click Undo last request again to go ahead.".into(),
                error: true,
            });
        } else {
            let (json, studio) = app.claude.before.take().unwrap();
            app.claude.confirm_revert = false;
            if let Ok(ws) = crate::workspace::Workspace::from_json(&json) {
                app.apply_agent_workspace(ws, studio);
                app.claude.entries.push(Entry::Notice { text: "Undid the last request's changes.".into(), error: false });
            }
        }
    }
}

fn entry(ui: &mut Ui, e: &Entry) {
    match e {
        Entry::User(t) => {
            ui.add_space(6.0);
            Frame::new().fill(theme::sel_fill()).corner_radius(8).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(t).size(12.5).color(theme::text())).selectable(true));
            });
        }
        Entry::Assistant(t) => {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(t.trim()).size(12.5).color(theme::text())).selectable(true));
        }
        Entry::Thinking(t) => {
            egui::CollapsingHeader::new(RichText::new("Thinking").size(11.5).color(theme::muted())).id_salt(t.as_ptr()).show(ui, |ui| {
                ui.label(RichText::new(t.trim()).size(11.5).italics().color(theme::muted()));
            });
        }
        Entry::Tool { name, detail, result, image } => {
            let (mark, col) = match result {
                None => ("…", theme::muted()),
                Some((true, _)) => ("✓", theme::ok_green()),
                Some((false, _)) => ("✗", theme::error()),
            };
            let resp = ui.horizontal(|ui| {
                ui.label(RichText::new(mark).size(11.5).color(col));
                ui.label(RichText::new(name).size(11.5).family(theme::medium()).color(theme::text()));
                if let Some((_, s)) = result {
                    ui.add(egui::Label::new(RichText::new(s).size(11.0).color(if col == theme::error() { col } else { theme::muted() })).truncate());
                }
            });
            resp.response.on_hover_text(detail);
            if let Some(tex) = image {
                let w = ui.available_width().min(320.0);
                let size = tex.size_vec2();
                ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))).corner_radius(6));
            }
        }
        Entry::Notice { text, error } => {
            ui.label(RichText::new(text).size(11.5).color(if *error { theme::error() } else { theme::muted() }));
        }
    }
}

/// Send the typed request, with the open document, to Claude.
fn send(app: &mut App, ctx: &egui::Context) {
    if app.sketch.is_some() {
        crate::sketcher::finish(app);
    }
    if app.dialog.is_some() {
        app.dialog_ok();
    }
    app.sync_ws();
    let ws = app.ws.to_json();
    let studio = app.cur_studio;
    let text = std::mem::take(&mut app.claude.input).trim().to_string();
    let context = editor_context(app);
    let c = &mut app.claude;
    let worker = c.worker.get_or_insert_with(|| Worker::start(ctx.clone()));
    c.before = Some((ws.clone(), studio));
    c.changed = false;
    c.after_hash = None;
    c.confirm_revert = false;
    c.busy = true;
    c.entries.push(Entry::User(text.clone()));
    let _ = worker.tx.send(ToWorker::Turn { text, context, ws, studio, settings: c.settings.clone() });
}

/// What Claude is told about the editor with each request.
fn editor_context(app: &App) -> String {
    use crate::workspace::Tab;
    let tab = match app.tab {
        Tab::Studio(i) => format!("Part Studio {i} \"{}\"", app.ws.studios.get(i).map(|s| s.name.as_str()).unwrap_or("")),
        Tab::Assembly(i) => format!("Assembly {i} \"{}\"", app.ws.assemblies.get(i).map(|s| s.name.as_str()).unwrap_or("")),
        Tab::Drawing(i) => format!("Drawing {i} \"{}\"", app.ws.drawings.get(i).map(|s| s.name.as_str()).unwrap_or("")),
        Tab::Cam(i) => format!("Manufacture {i} \"{}\"", app.ws.cams.get(i).map(|s| s.name.as_str()).unwrap_or("")),
        Tab::Sim(i) => format!("Simulation {i} \"{}\"", app.ws.sims.get(i).map(|s| s.name.as_str()).unwrap_or("")),
        Tab::Pcb(i) => format!("PCB {i} \"{}\"", app.ws.pcbs.get(i).map(|s| s.name.as_str()).unwrap_or("")),
    };
    let mut lines = vec![format!("Open tab: {tab}"), format!("Units: {}", app.doc.units.label())];
    match &app.path {
        Some(p) => lines.push(format!("Document file: {}", p.display())),
        None => {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| std::path::PathBuf::from(h).display().to_string()).unwrap_or_default();
            lines.push(format!("Document not saved yet; home folder: {home}"));
        }
    }
    if !app.picks.is_empty() && matches!(app.tab, Tab::Studio(_)) {
        lines.push(format!("Selected: {}", serde_json::to_string(&app.picks).unwrap_or_default()));
    } else {
        lines.push("Selected: nothing".into());
    }
    lines.join("\n")
}

fn poll(app: &mut App, ctx: &egui::Context) {
    let Some(w) = &app.claude.worker else { return };
    let events: Vec<FromWorker> = w.rx.try_iter().collect();
    for ev in events {
        let c = &mut app.claude;
        match ev {
            FromWorker::Text(t) => match c.entries.last_mut() {
                Some(Entry::Assistant(s)) => s.push_str(&t),
                _ => c.entries.push(Entry::Assistant(t)),
            },
            FromWorker::Thinking(t) => match c.entries.last_mut() {
                Some(Entry::Thinking(s)) => s.push_str(&t),
                _ => c.entries.push(Entry::Thinking(t)),
            },
            FromWorker::ToolStart { name, detail } => c.entries.push(Entry::Tool { name, detail, result: None, image: None }),
            FromWorker::ToolDone { ok, summary, image } => {
                let tex = image.and_then(|png| image::load_from_memory(&png).ok()).map(|img| {
                    let rgba = img.to_rgba8();
                    let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
                    ctx.load_texture("claude-render", ci, Default::default())
                });
                if let Some(Entry::Tool { result, image, .. }) = c.entries.iter_mut().rev().find(|e| matches!(e, Entry::Tool { result: None, .. })) {
                    *result = Some((ok, summary));
                    *image = tex;
                }
            }
            FromWorker::Workspace { json, studio } => {
                c.changed = true;
                match crate::workspace::Workspace::from_json(&json) {
                    Ok(ws) => app.apply_agent_workspace(ws, studio),
                    Err(e) => c.entries.push(Entry::Notice { text: format!("Could not apply Claude's change: {e}"), error: true }),
                }
            }
            FromWorker::Notice(t) => c.entries.push(Entry::Notice { text: t, error: false }),
            FromWorker::Attempt => {
                let last = match c.entries.last() {
                    Some(Entry::Assistant(s) | Entry::Thinking(s)) => Some(s.len()),
                    _ => None,
                };
                c.attempt = (c.entries.len(), last);
            }
            FromWorker::Rewind => {
                // the request is sent again: its partial text would show twice
                let (n, last) = c.attempt;
                c.entries.truncate(n);
                if let (Some(l), Some(Entry::Assistant(s) | Entry::Thinking(s))) = (last, c.entries.last_mut()) {
                    s.truncate(l);
                }
            }
            FromWorker::Done { error } => {
                c.busy = false;
                app.sync_ws();
                let h = app.content_hash();
                let c = &mut app.claude;
                c.after_hash = Some(h);
                if let Some(e) = error {
                    c.entries.push(Entry::Notice { text: e, error: true });
                }
            }
        }
    }
}
