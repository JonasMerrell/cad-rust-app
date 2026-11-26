//! The Claude chat panel: type a design request, Claude edits the document.

use egui::{Align, Frame, Key, Layout, Margin, RichText, ScrollArea, Ui};

use crate::app::App;
use crate::claude::{Auth, EFFORTS, FromWorker, MODELS, Settings, ToWorker, Worker};
use crate::claude_chats::{self, Line};
use crate::theme;

enum Entry {
    User(String),
    Assistant(String),
    Thinking(String),
    Tool { name: String, detail: String, result: Option<(bool, String)>, image: Option<egui::TextureHandle> },
    Notice { text: String, error: bool },
    /// A pencil drawing sent with the request after it.
    Sketch(egui::TextureHandle),
}

/// A pencil drawing waiting to go with the next request (`crate::notes`).
pub struct SketchAttachment {
    png: Vec<u8>,
    tex: egui::TextureHandle,
    /// The strokes and the faces they cross, in words, for Claude.
    text: String,
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
    /// Scroll the history to its end on the next frame.
    scroll_to_end: bool,
    /// Part Studios that got their undo step in this request: one per request, not per tool call.
    checkpointed: Vec<usize>,
    /// The chat being had, as saved in the document's folder (`claude_chats`): its id once saved
    /// (or about to be), when it started, and what Claude continues from.
    chat_id: Option<String>,
    chat_created: u64,
    context: claude_chats::Context,
    /// The document's saved chats, newest first, and the document they were listed for.
    chats: Vec<claude_chats::Meta>,
    chats_for: Option<Option<std::path::PathBuf>>,
    /// Showing the chat list instead of the conversation.
    list_open: bool,
    /// A chat whose Delete was clicked once.
    confirm_delete: Option<String>,
    /// The pencil drawing for the next request.
    pub sketch: Option<SketchAttachment>,
    focus_input: bool,
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
            scroll_to_end: false,
            checkpointed: vec![],
            chat_id: None,
            chat_created: 0,
            context: Default::default(),
            chats: vec![],
            chats_for: None,
            list_open: false,
            confirm_delete: None,
            sketch: None,
            focus_input: false,
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
            Auth::Console | Auth::ClaudeCode | Auth::Gemini | Auth::Codex => true,
        }
    }
}

/// Put a pencil drawing (PNG of the viewport) on the next request, and open the panel for it.
pub fn attach_sketch(app: &mut App, ctx: &egui::Context, png: Vec<u8>, text: String) {
    let Ok(img) = image::load_from_memory(&png) else { return };
    let rgba = img.to_rgba8();
    let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
    let tex = ctx.load_texture("claude-sketch", ci, Default::default());
    let c = &mut app.claude;
    c.sketch = Some(SketchAttachment { png, tex, text });
    c.open = true;
    c.focus_input = true;
    if c.input.trim().is_empty() {
        c.input.clear();
    }
}

pub fn toggle(app: &mut App) {
    app.claude.open = !app.claude.open;
}

/// Handle what the worker sent, then draw the panel (when open).
pub fn panel(app: &mut App, ui: &mut Ui) {
    poll(app, ui.ctx());
    if app.claude.chats_for.as_ref() != Some(&app.path) {
        refresh_chats(app);
    }
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
        } else if app.claude.list_open && !app.claude.busy {
            ScrollArea::vertical().id_salt("claude-chats").auto_shrink([false, false]).show(ui, |ui| chat_list(app, ui));
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
                ui.label(RichText::new(format!("{} is changing the document", app.claude.settings.assistant())).size(12.5).color(theme::text()));
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
        ui.label(RichText::new(app.claude.settings.assistant()).family(theme::medium()).size(14.0).color(theme::text()));
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
                new_chat(c);
            }
            if c.configured()
                && !c.busy
                && (!c.chats.is_empty() || c.list_open)
                && ui.add(egui::Button::image(crate::icons::image("history", 15.0).tint(theme::text())).selected(c.list_open)).on_hover_text("Earlier chats").clicked()
            {
                c.list_open = !c.list_open;
                c.confirm_delete = None;
            }
        });
    });
}

/// Run a check that starts a program (and may wait on the network) without freezing the window.
fn off_thread(ctx: &egui::Context, f: impl FnOnce() -> Result<String, String> + Send + 'static) -> std::sync::mpsc::Receiver<Result<String, String>> {
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
        RichText::new(format!("Design in plain language: describe a part or a change, and {} builds it in this document with the same tools as the MCP server.", c.settings.assistant()))
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
    ui.radio_value(&mut auth, Auth::Gemini, "Gemini CLI (your Google account / Gemini plan)");
    ui.radio_value(&mut auth, Auth::Codex, "Codex CLI (your ChatGPT plan)");
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
        Auth::Gemini | Auth::Codex => {
            let cli = crate::claude::Cli::of(c.settings.auth).unwrap();
            muted(
                ui,
                match cli {
                    crate::claude::Cli::Gemini => "Runs Google's Gemini CLI on this computer with its own sign-in, so requests use your Google account's Gemini plan. Install it (npm install -g @google/gemini-cli) and run gemini once to sign in with Google. In the chat it can only use rCAD's tools.",
                    crate::claude::Cli::Codex => "Runs OpenAI's Codex CLI on this computer with its own sign-in, so requests use your ChatGPT plan. Install it (npm install -g @openai/codex) and run codex login to sign in with ChatGPT. In the chat it can only use rCAD's tools.",
                },
            );
            if ui.add_enabled(c.check.is_none(), egui::Button::new("Check")).clicked() {
                c.check = Some(off_thread(ui.ctx(), move || crate::claude::cli_version(cli)));
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
    if let Some(cli) = crate::claude::Cli::of(c.settings.auth) {
        ui.horizontal(|ui| {
            theme::field_style(ui);
            let (field, hint) = match cli {
                crate::claude::Cli::Gemini => (&mut c.settings.gemini_model, "auto (the CLI's choice)"),
                crate::claude::Cli::Codex => (&mut c.settings.codex_model, "Codex's default"),
            };
            changed |= ui.add(egui::TextEdit::singleline(field).hint_text(hint).desired_width(170.0)).changed();
            if cli == crate::claude::Cli::Codex {
                egui::ComboBox::from_id_salt("claude-effort").selected_text(format!("Effort: {}", c.settings.effort)).width(110.0).show_ui(ui, |ui| {
                    for e in EFFORTS {
                        changed |= ui.selectable_value(&mut c.settings.effort, e.to_string(), *e).changed();
                    }
                });
            }
        });
        muted(ui, match cli {
            crate::claude::Cli::Gemini => "A Gemini model id (e.g. gemini-3.1-pro-preview), or leave it empty for auto.",
            crate::claude::Cli::Codex => "A model id (e.g. gpt-5.5), or leave it empty for Codex's default. Effort sets its reasoning effort (max is xhigh).",
        });
        if changed {
            let _ = crate::claude::save_settings(&c.settings);
        }
        if c.configured() {
            ui.add_space(10.0);
            if ui.button("Done").clicked() {
                c.settings_open = false;
            }
        }
        return;
    }
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
    if !crate::claude::WORKERS.iter().any(|w| w.1 == c.settings.model) {
        ui.add_space(6.0);
        let name = MODELS.iter().find(|m| m.0 == c.settings.model).map(|m| m.1.trim_start_matches("Claude ")).unwrap_or("This model").to_string();
        changed |= ui.checkbox(&mut c.settings.delegate, format!("{name} plans, Sonnet and Haiku do the work")).changed();
        muted(ui, &format!("{name} looks at the document and writes exact steps; cheaper models make the edits. Costs much less than {name} doing every tool call."));
    }
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

/// The tallest the request box grows before it scrolls itself.
const INPUT_MAX_H: f32 = 150.0;

fn chat(app: &mut App, ui: &mut Ui) {
    // The request box and its buttons sit in a panel along the bottom so they stay put however
    // long the conversation or the typed request gets; the history scrolls in what is left.
    let input = egui::Panel::bottom("claude-input").frame(Frame::NONE).resizable(false).show_separator_line(false).show(ui, |ui| {
        ui.separator();
        let (mut go, mut revert) = (false, false);
        let c = &mut app.claude;
        theme::field_style(ui);
        // the pencil drawing that goes with the request
        let mut drop_sketch = false;
        if let Some(sk) = &c.sketch {
            let size = sk.tex.size_vec2();
            let w = ui.available_width().min(180.0 * size.x / size.y.max(1.0));
            ui.add(egui::Image::new(&sk.tex).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))).corner_radius(4));
            ui.horizontal(|ui| {
                let faces = sk.text.lines().filter(|l| l.starts_with("- ")).count();
                ui.label(RichText::new(format!("Pencil drawing · {faces} face{} under the strokes", if faces == 1 { "" } else { "s" })).size(11.5).color(theme::muted()));
                drop_sketch = ui.small_button("Remove").clicked();
            });
        }
        if drop_sketch {
            c.sketch = None;
        }
        let has_sketch = c.sketch.is_some();
        // a long request scrolls inside its box instead of pushing the buttons off the panel
        let r = ScrollArea::vertical()
            .id_salt("claude-input")
            .max_height(INPUT_MAX_H)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut c.input)
                        .hint_text(format!("Ask {} to design or change something… (Enter sends, Shift+Enter for a new line)", c.settings.assistant()))
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                )
            })
            .inner;
        if std::mem::take(&mut c.focus_input) {
            r.request_focus();
        }
        let enter = r.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
        if enter {
            // drop the newline the key press added
            while c.input.ends_with('\n') {
                c.input.pop();
            }
        }
        ui.horizontal(|ui| {
            if c.before.is_some() && c.changed && !c.busy {
                revert = ui.small_button("Undo last request").on_hover_text("Put the document back the way it was before Claude's last request").clicked();
            } else {
                let line = match crate::claude::Cli::of(c.settings.auth) {
                    Some(cli) => {
                        let m = c.settings.cli_model(cli).trim();
                        let m = if m.is_empty() { "default model" } else { m };
                        match cli {
                            crate::claude::Cli::Codex => format!("Codex · {m} · {}", c.settings.effort),
                            crate::claude::Cli::Gemini => format!("Gemini CLI · {m}"),
                        }
                    }
                    None => {
                        let model = MODELS.iter().find(|m| m.0 == c.settings.model).map(|m| m.1).unwrap_or("");
                        let workers = if c.settings.delegates() { " + Sonnet/Haiku" } else { "" };
                        format!("{model}{workers} · {}", c.settings.effort)
                    }
                };
                ui.label(RichText::new(line).size(11.0).color(theme::muted()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if c.busy {
                    if ui.button("Stop").clicked() {
                        if let Some(w) = &c.worker {
                            w.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                } else {
                    let ready = !c.input.trim().is_empty() || has_sketch;
                    go = (ui.add_enabled(ready, egui::Button::new("Send")).clicked() || enter) && ready;
                }
            });
        });
        (go, revert)
    });
    let (go, revert) = input.inner;
    let jump = std::mem::take(&mut app.claude.scroll_to_end);
    ScrollArea::vertical().id_salt("claude-chat").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if app.claude.entries.is_empty() {
            if !app.claude.chats.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new("Continue a chat about this document:").size(12.0).color(theme::muted()));
                let mut open = None;
                for m in app.claude.chats.iter().take(3) {
                    if chat_row(ui, m).clicked() {
                        open = Some(m.id.clone());
                    }
                }
                if app.claude.chats.len() > 3 && ui.small_button(format!("All {} chats…", app.claude.chats.len())).clicked() {
                    app.claude.list_open = true;
                }
                if let Some(id) = open {
                    resume(app, ui.ctx(), &id);
                }
                ui.add_space(6.0);
            }
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
        if jump {
            // a new request: show it even when the history was scrolled up
            ui.scroll_to_cursor(Some(Align::BOTTOM));
        }
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
                app.apply_agent_workspace(ws, studio, true);
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
        Entry::Sketch(tex) => {
            ui.add_space(6.0);
            let w = ui.available_width().min(320.0);
            let size = tex.size_vec2();
            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))).corner_radius(6));
            });
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
    let mut text = std::mem::take(&mut app.claude.input).trim().to_string();
    let mut context = editor_context(app);
    let sketch = app.claude.sketch.take();
    if let Some(sk) = &sketch {
        if text.is_empty() {
            text = "Look at what I drew on the part.".into();
        }
        context.push_str("\n");
        context.push_str(&sk.text);
    }
    let c = &mut app.claude;
    let worker = c.worker.get_or_insert_with(|| Worker::start(ctx.clone()));
    c.before = Some((ws.clone(), studio));
    c.changed = false;
    c.after_hash = None;
    c.confirm_revert = false;
    c.checkpointed.clear();
    c.busy = true;
    let images: Vec<Vec<u8>> = sketch
        .map(|sk| {
            c.entries.push(Entry::Sketch(sk.tex));
            sk.png
        })
        .into_iter()
        .collect();
    c.entries.push(Entry::User(text.clone()));
    c.scroll_to_end = true;
    let _ = worker.tx.send(ToWorker::Turn { text, context, ws, studio, settings: c.settings.clone(), images });
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
    if matches!(app.tab, Tab::Studio(_)) {
        if let Some(n) = crate::notes::context(app) {
            lines.push(n);
        }
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
                    Ok(ws) => {
                        let cur = app.cur_studio;
                        let first = !app.claude.checkpointed.contains(&cur);
                        if app.apply_agent_workspace(ws, studio, first) {
                            app.claude.checkpointed.push(cur);
                        }
                    }
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
            FromWorker::State { messages, cc_session, planned } => c.context = claude_chats::Context { messages, claude_code_session: cc_session, planned },
            FromWorker::Done { error } => {
                c.busy = false;
                app.sync_ws();
                let h = app.content_hash();
                let c = &mut app.claude;
                c.after_hash = Some(h);
                if let Some(e) = error {
                    c.entries.push(Entry::Notice { text: e, error: true });
                }
                save_chat(app);
            }
        }
    }
}

// ------------------------------------------------------------ saved chats

fn line_of(e: &Entry) -> Line {
    match e {
        Entry::User(t) => Line::User { text: t.clone() },
        Entry::Assistant(t) => Line::Assistant { text: t.clone() },
        Entry::Thinking(t) => Line::Thinking { text: t.clone() },
        Entry::Tool { name, detail, result, .. } => Line::Tool { name: name.clone(), detail: detail.clone(), ok: result.as_ref().map(|r| r.0), summary: result.as_ref().map(|r| r.1.clone()).unwrap_or_default() },
        Entry::Notice { text, error } => Line::Notice { text: text.clone(), error: *error },
        // the picture itself is in the conversation Claude continues from
        Entry::Sketch(_) => Line::Notice { text: "(a pencil drawing went with the next message)".into(), error: false },
    }
}

fn entry_of(l: &Line) -> Entry {
    match l {
        Line::User { text } => Entry::User(text.clone()),
        Line::Assistant { text } => Entry::Assistant(text.clone()),
        Line::Thinking { text } => Entry::Thinking(text.clone()),
        Line::Tool { name, detail, ok, summary } => Entry::Tool { name: name.clone(), detail: detail.clone(), result: ok.map(|ok| (ok, summary.clone())), image: None },
        Line::Notice { text, error } => Entry::Notice { text: text.clone(), error: *error },
    }
}

fn refresh_chats(app: &mut App) {
    let c = &mut app.claude;
    c.chats = app.path.as_deref().map(claude_chats::list).unwrap_or_default();
    c.chats_for = Some(app.path.clone());
    if c.chats.is_empty() {
        c.list_open = false;
    }
}

/// Start a new conversation (the one before is saved already).
fn new_chat(c: &mut ClaudeUi) {
    c.entries.clear();
    c.before = None;
    c.changed = false;
    c.chat_id = None;
    c.context = Default::default();
    c.list_open = false;
    if let Some(w) = &c.worker {
        let _ = w.tx.send(ToWorker::Reset);
    }
}

/// Save the conversation with the document (when it has a file; an unsaved document's chat is
/// saved with it the first time it is saved).
fn save_chat(app: &mut App) {
    let Some(doc) = app.path.clone() else { return };
    let c = &mut app.claude;
    if c.entries.is_empty() {
        return;
    }
    let id = match &c.chat_id {
        Some(id) => id.clone(),
        None => {
            c.chat_created = claude_chats::now();
            c.chat_id.insert(claude_chats::new_id()).clone()
        }
    };
    let mut meta = claude_chats::Meta {
        format: 0,
        id,
        document: String::new(),
        title: String::new(),
        created: c.chat_created,
        updated: 0,
        model: c.settings.model.clone(),
        lines: c.entries.iter().map(line_of).collect(),
    };
    if let Err(e) = claude_chats::save(&doc, &mut meta, &c.context) {
        c.entries.push(Entry::Notice { text: format!("Could not save this chat: {e}"), error: true });
    }
    refresh_chats(app);
}

/// Pick a saved chat up where it was left.
fn resume(app: &mut App, ctx: &egui::Context, id: &str) {
    let Some(doc) = app.path.clone() else { return };
    let c = &mut app.claude;
    if c.busy {
        return;
    }
    match claude_chats::load(&doc, id) {
        Ok((meta, context)) => {
            let worker = c.worker.get_or_insert_with(|| Worker::start(ctx.clone()));
            let recap = Some(claude_chats::recap(&meta.lines)).filter(|r| !r.is_empty());
            let _ = worker.tx.send(ToWorker::Load { messages: context.messages.clone(), cc_session: context.claude_code_session.clone(), planned: context.planned, recap });
            c.entries = meta.lines.iter().map(entry_of).collect();
            c.chat_id = Some(meta.id);
            c.chat_created = meta.created;
            c.context = context;
            c.before = None;
            c.changed = false;
            c.list_open = false;
            c.scroll_to_end = true;
        }
        Err(e) => c.entries.push(Entry::Notice { text: format!("Could not open that chat: {e}"), error: true }),
    }
}

/// Another document was opened (or a new one started): its own chats, not this conversation.
pub fn document_opened(app: &mut App) {
    let c = &mut app.claude;
    if c.busy {
        // what it is still doing belongs to the document that was open
        if let Some(w) = c.worker.take() {
            w.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        c.busy = false;
    }
    new_chat(c);
    c.chats_for = None;
}

/// The document was saved (perhaps somewhere new): its chat goes with it.
pub fn document_saved(app: &mut App) {
    if !app.claude.busy {
        save_chat(app);
    }
    app.claude.chats_for = None;
}

fn chat_row(ui: &mut Ui, m: &claude_chats::Meta) -> egui::Response {
    let n = m.requests();
    let sub = format!("{} · {n} request{}", claude_chats::ago(m.updated), if n == 1 { "" } else { "s" });
    let mut job = egui::text::LayoutJob::default();
    RichText::new(format!("{}\n", m.title)).size(12.0).color(theme::text()).append_to(&mut job, ui.style(), egui::FontSelection::Default, Align::LEFT);
    RichText::new(sub).size(10.5).color(theme::muted()).append_to(&mut job, ui.style(), egui::FontSelection::Default, Align::LEFT);
    ui.add(egui::Button::new(job).wrap().fill(theme::panel_2()).min_size(egui::vec2(ui.available_width(), 0.0)))
}

fn chat_list(app: &mut App, ui: &mut Ui) {
    ui.label(RichText::new("CHATS ABOUT THIS DOCUMENT").family(theme::medium()).color(theme::muted()).size(10.5));
    ui.label(RichText::new("Saved in the project folder on this computer (not synced to GitHub).").size(11.0).color(theme::muted()));
    ui.add_space(4.0);
    let (mut open, mut delete) = (None, None);
    let cur = app.claude.chat_id.clone();
    for m in &app.claude.chats {
        ui.horizontal(|ui| {
            let confirming = app.claude.confirm_delete.as_deref() == Some(m.id.as_str());
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                let del = if confirming {
                    ui.small_button(RichText::new("Delete").color(theme::error())).on_hover_text("Click again to delete this chat")
                } else {
                    ui.add(egui::Button::image(crate::icons::image("delete", 14.0).tint(theme::muted())).frame(false)).on_hover_text("Delete this chat")
                };
                if del.clicked() {
                    delete = Some((m.id.clone(), confirming));
                }
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.set_width(ui.available_width());
                    let n = m.requests();
                    let current = if cur.as_deref() == Some(m.id.as_str()) { " · open now" } else { "" };
                    let mut job = egui::text::LayoutJob::default();
                    RichText::new(format!("{}\n", m.title)).size(12.0).color(theme::text()).append_to(&mut job, ui.style(), egui::FontSelection::Default, Align::LEFT);
                    RichText::new(format!("{} · {n} request{}{current}", claude_chats::ago(m.updated), if n == 1 { "" } else { "s" })).size(10.5).color(theme::muted()).append_to(
                        &mut job,
                        ui.style(),
                        egui::FontSelection::Default,
                        Align::LEFT,
                    );
                    if ui.add(egui::Button::new(job).wrap().fill(theme::panel_2()).min_size(egui::vec2(ui.available_width(), 0.0))).clicked() {
                        open = Some(m.id.clone());
                    }
                });
            });
        });
    }
    if let Some((id, confirmed)) = delete {
        if confirmed {
            if let Some(doc) = app.path.clone() {
                let _ = claude_chats::delete(&doc, &id);
            }
            if app.claude.chat_id.as_deref() == Some(id.as_str()) {
                // the open conversation carries on, as a chat of its own again
                app.claude.chat_id = None;
            }
            app.claude.confirm_delete = None;
            refresh_chats(app);
        } else {
            app.claude.confirm_delete = Some(id);
        }
    }
    if let Some(id) = open {
        if app.claude.chat_id.as_deref() == Some(id.as_str()) {
            app.claude.list_open = false;
        } else {
            resume(app, ui.ctx(), &id);
        }
    }
}
