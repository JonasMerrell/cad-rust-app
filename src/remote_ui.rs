//! The "Open from another computer" window (pairing with netdash's rCAD receiver, its list of
//! documents), saving an open remote document back after each save, the warning when the other
//! computer's copy changes underneath, and the reload / keep mine / save as copy prompt when a save
//! meets such a change. The network work is in `remote.rs` and runs on threads.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, UNIX_EPOCH};

use egui::{Align, Frame, Layout, Margin, RichText};

use crate::app::{App, ToastKind};
use crate::icons;
use crate::remote::{self, Client, Opened, Pushed, RemoteDoc, SaveError, Server};
use crate::theme;

/// How often an open remote document is checked for changes made over there.
const POLL: Duration = Duration::from_secs(5);

type Rx<T> = Option<Receiver<T>>;

/// A save that met a change made on the other computer, waiting for the user's choice.
struct Conflict {
    local: PathBuf,
    /// The remote copy's etag now (what "keep mine" may replace).
    theirs: Option<String>,
    copy_name: String,
}

#[derive(Default)]
pub struct RemoteUi {
    /// The window.
    pub open: bool,
    server: Option<Server>,
    loaded: bool,
    link_text: String,
    addr: String,
    token: String,
    pairing: Rx<Result<(Server, Vec<RemoteDoc>), String>>,
    pair_error: Option<String>,
    docs: Option<Result<Vec<RemoteDoc>, String>>,
    docs_rx: Rx<Result<Vec<RemoteDoc>, String>>,
    filter: String,
    opening: Option<(String, Receiver<Result<Opened, String>>)>,
    /// The open document, when it is a working copy of a remote one.
    current: Option<PathBuf>,
    saving: Option<(PathBuf, Receiver<Result<Pushed, SaveError>>)>,
    /// Saved again while the last save was still going over.
    save_again: Option<PathBuf>,
    conflict: Option<Conflict>,
    /// A reload / keep mine / copy under way: the document to open when it's done (None: stay).
    resolving: Option<Receiver<Result<Option<PathBuf>, String>>>,
    polling: Rx<Result<Option<String>, String>>,
    polled: Option<Instant>,
    /// The remote copy changed (its etag), and the etag the user said "not now" to.
    changed: Option<String>,
    dismissed: Option<String>,
    /// Couldn't reach the other computer at the last check.
    offline: Option<String>,
    /// For waking the UI when a thread is done.
    ctx: Option<egui::Context>,
}

fn ensure_loaded(app: &mut App) {
    if !app.remote.loaded {
        app.remote.server = remote::load_server();
        app.remote.loaded = true;
    }
}

/// The paired computer's name for menus ("netdash"), or None when there isn't one.
pub fn paired_name(app: &mut App) -> Option<String> {
    ensure_loaded(app);
    app.remote.server.as_ref().map(|s| s.label())
}

pub fn open_window(app: &mut App) {
    ensure_loaded(app);
    app.remote.open = true;
    app.remote.pair_error = None;
    if app.remote.server.is_some() && app.remote.docs_rx.is_none() {
        load_docs(app);
    }
}

fn spawn<T: Send + 'static>(ctx: Option<&egui::Context>, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    let ctx = ctx.cloned();
    std::thread::spawn(move || {
        let _ = tx.send(f());
        if let Some(c) = ctx {
            c.request_repaint();
        }
    });
    rx
}

fn with_client<T: Send + 'static>(app: &App, f: impl FnOnce(&Client) -> T + Send + 'static, err: impl FnOnce(String) -> T + Send + 'static) -> Option<Receiver<T>> {
    let s = app.remote.server.clone()?;
    Some(spawn(app.remote.ctx.as_ref(), move || match Client::connect(&s) {
        Ok(c) => f(&c),
        Err(e) => err(e),
    }))
}

fn load_docs(app: &mut App) {
    app.remote.docs_rx = with_client(app, |c| c.documents(), Err);
}

/// A document was opened: follow it when it is a working copy of a remote one.
pub fn document_opened(app: &mut App, p: &Path) {
    let r = &mut app.remote;
    r.current = remote::link_for(p).map(|_| p.to_path_buf());
    r.changed = None;
    r.dismissed = None;
    r.offline = None;
    r.conflict = None;
    // check right away (a working copy opened from the recent list may be behind)
    r.polled = None;
}

/// After a save to `p`: send it back when it is a working copy. A note for the "Saved" message.
pub fn after_save(app: &mut App, p: &Path) -> String {
    let Some(link) = remote::link_for(p) else {
        app.remote.current = None;
        return String::new();
    };
    ensure_loaded(app);
    app.remote.current = Some(p.to_path_buf());
    let host = app.remote.server.as_ref().map(|s| s.label()).unwrap_or(link.host.clone());
    if app.remote.server.is_none() {
        return format!(" here only: pair with {host} again (File > Open from another computer) to send it");
    }
    if app.remote.saving.is_some() {
        app.remote.save_again = Some(p.to_path_buf());
    } else {
        start_push(app, p.to_path_buf(), None);
    }
    " (sending it…)".into()
}

fn start_push(app: &mut App, p: PathBuf, base: Option<String>) {
    let q = p.clone();
    if let Some(rx) = with_client(app, move |c| remote::push(c, &q, base.as_deref()), |e| Err(SaveError::Other(e))) {
        app.remote.saving = Some((p, rx));
    }
}

/// Runs every frame (on the start screen too).
pub fn frame(app: &mut App, ctx: &egui::Context) {
    if app.remote.ctx.is_none() {
        app.remote.ctx = Some(ctx.clone());
    }
    poll_results(app);
    poll_remote(app, ctx);
    if app.remote.open {
        window(app, ctx);
    }
    if app.remote.conflict.is_some() {
        conflict_window(app, ctx);
    } else if app.remote.changed.is_some() && app.remote.changed != app.remote.dismissed && app.remote.saving.is_none() {
        changed_window(app, ctx);
    }
}

fn host_name(app: &App) -> String {
    app.remote.server.as_ref().map(|s| s.label()).unwrap_or_else(|| "the other computer".into())
}

fn poll_results(app: &mut App) {
    if let Some(rx) = &app.remote.pairing {
        if let Ok(res) = rx.try_recv() {
            app.remote.pairing = None;
            match res {
                Ok((s, docs)) => {
                    if let Err(e) = remote::save_server(Some(&s)) {
                        app.flash(format!("Paired, but couldn't remember it: {e}"), ToastKind::Error);
                    }
                    app.remote.server = Some(s);
                    app.remote.docs = Some(Ok(docs));
                    app.remote.token.clear();
                    app.remote.link_text.clear();
                }
                Err(e) => app.remote.pair_error = Some(e),
            }
        }
    }
    if let Some(rx) = &app.remote.docs_rx {
        if let Ok(res) = rx.try_recv() {
            app.remote.docs_rx = None;
            app.remote.docs = Some(res);
        }
    }
    if let Some((_, rx)) = &app.remote.opening {
        if let Ok(res) = rx.try_recv() {
            app.remote.opening = None;
            match res {
                Ok(o) => {
                    app.remote.open = false;
                    let host = host_name(app);
                    app.guard(crate::session::Pending::OpenPath(o.local.clone()));
                    if o.kept_local {
                        app.flash_info(format!("This copy has a save that never reached {host}: it was kept, and goes over the next time you save"), Some(Duration::from_secs(6)));
                    }
                }
                Err(e) => app.flash(format!("Could not open it: {e}"), ToastKind::Error),
            }
        }
    }
    if let Some((p, rx)) = &app.remote.saving {
        if let Ok(res) = rx.try_recv() {
            let p = p.clone();
            app.remote.saving = None;
            let host = host_name(app);
            match res {
                Ok(done) => {
                    app.remote.changed = None;
                    app.remote.offline = None;
                    let mut note = String::new();
                    if done.chat_conflicts > 0 {
                        note = format!(" ({} chat file{} changed there too and kept {host}'s)", done.chat_conflicts, if done.chat_conflicts == 1 { "" } else { "s" });
                    }
                    app.flash_info(format!("Saved to {host}{note}"), Some(Duration::from_secs(3)));
                }
                Err(SaveError::Stale(theirs)) => {
                    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let me = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "mine".into());
                    app.remote.conflict = Some(Conflict { local: p.clone(), theirs, copy_name: format!("{stem} ({me})") });
                    app.remote.save_again = None;
                }
                Err(SaveError::Busy(e)) => app.flash(format!("Saved here, not on {host} yet: {e}"), ToastKind::Error),
                Err(SaveError::Other(e)) => app.flash(format!("Saved here, but not sent to {host}: {e}. It goes over the next time you save."), ToastKind::Error),
            }
            if let Some(next) = app.remote.save_again.take() {
                start_push(app, next, None);
            }
        }
    }
    if let Some(rx) = &app.remote.resolving {
        if let Ok(res) = rx.try_recv() {
            app.remote.resolving = None;
            match res {
                Ok(Some(open)) => {
                    // the user chose what to keep: no "save changes?" for what is replaced
                    app.open_path(open);
                }
                Ok(None) => {
                    app.remote.changed = None;
                    let host = host_name(app);
                    app.flash_info(format!("Saved to {host}"), Some(Duration::from_secs(3)));
                }
                Err(e) => app.flash(e, ToastKind::Error),
            }
        }
    }
    if let Some(rx) = &app.remote.polling {
        if let Ok(res) = rx.try_recv() {
            app.remote.polling = None;
            app.remote.polled = Some(Instant::now());
            match res {
                Ok(t) => {
                    app.remote.offline = None;
                    app.remote.changed = t;
                }
                Err(e) => app.remote.offline = Some(e),
            }
        }
    }
}

/// Check the open remote document for changes made over there every few seconds.
fn poll_remote(app: &mut App, ctx: &egui::Context) {
    let Some(p) = app.remote.current.clone() else { return };
    if app.path.as_deref() != Some(p.as_path()) {
        app.remote.current = None;
        return;
    }
    ensure_loaded(app);
    let busy = app.remote.polling.is_some() || app.remote.saving.is_some() || app.remote.resolving.is_some() || app.remote.conflict.is_some();
    let due = app.remote.polled.is_none_or(|t| t.elapsed() >= POLL);
    if !busy && due {
        if let Some(link) = remote::link_for(&p) {
            app.remote.polling = with_client(app, move |c| remote::check(c, &link), Err);
        }
    }
    ctx.request_repaint_after(POLL);
}

fn muted(ui: &mut egui::Ui, t: impl Into<String>) {
    ui.label(RichText::new(t.into()).size(11.5).color(theme::muted()));
}

fn ago(secs: u64) -> String {
    crate::home::when(UNIX_EPOCH + Duration::from_secs(secs))
}

fn window(app: &mut App, ctx: &egui::Context) {
    let mut open = true;
    let mut pair: Option<Server> = None;
    let mut unpair = false;
    let mut reload = false;
    let mut pick: Option<RemoteDoc> = None;
    let title = match &app.remote.server {
        Some(s) => format!("Open from {}", s.label()),
        None => "Open from another computer".into(),
    };
    egui::Window::new(title)
        .id(egui::Id::new("remote-docs"))
        .open(&mut open)
        .default_size([500.0, 520.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(250.0, 260.0))
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| {
            let r = &mut app.remote;
            match r.server.clone() {
                None => {
                    muted(ui, "Pair with the computer that keeps your rCAD projects (netdash's rCAD receiver, the one the phone app uses). Documents open in a copy here and every save goes back to it.");
                    ui.add_space(6.0);
                    theme::field_style(ui);
                    ui.label("Pairing link");
                    ui.add(egui::TextEdit::singleline(&mut r.link_text).hint_text("rcad://pair?url=…&token=…  or  http://…:8765/pair?t=…").desired_width(f32::INFINITY));
                    muted(ui, "On that computer: python3 capture/receiver/rcad_capture_receiver.py pair-url");
                    ui.add_space(6.0);
                    ui.label("Or its address and token");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut r.addr).hint_text("192.168.0.247").desired_width(170.0));
                        ui.add(egui::TextEdit::singleline(&mut r.token).hint_text("token (~/.config/rcad-capture/token)").password(true).desired_width(f32::INFINITY));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let busy = r.pairing.is_some();
                        if ui.add_enabled(!busy, egui::Button::new("Connect")).clicked() {
                            let parsed = if !r.link_text.trim().is_empty() {
                                remote::parse_pair(&r.link_text)
                            } else if !r.addr.trim().is_empty() && !r.token.trim().is_empty() {
                                Ok((vec![remote::normalize_url(&r.addr)], r.token.trim().to_string()))
                            } else {
                                Err("paste the pairing link, or give the address and token".into())
                            };
                            match parsed {
                                Ok((urls, token)) => pair = Some(Server { urls, token, host: String::new() }),
                                Err(e) => r.pair_error = Some(e),
                            }
                        }
                        if busy {
                            ui.spinner();
                            muted(ui, "Connecting…");
                        }
                    });
                    if let Some(e) = &r.pair_error {
                        ui.label(RichText::new(e).color(theme::error()).size(11.5));
                    }
                }
                Some(s) => {
                    ui.horizontal(|ui| {
                        theme::field_style(ui);
                        ui.add(icons::image("search", 14.0).tint(theme::muted()));
                        ui.add(egui::TextEdit::singleline(&mut r.filter).hint_text(format!("Search {}'s documents", s.label())).desired_width(240.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Unpair").on_hover_text(format!("Forget {} and its token", s.urls.join(", "))).clicked() {
                                unpair = true;
                            }
                            if ui.add_enabled(r.docs_rx.is_none(), egui::Button::new("Refresh")).clicked() {
                                reload = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    let busy = r.opening.is_some();
                    egui::ScrollArea::vertical().id_salt("remote-docs-list").auto_shrink([false, false]).max_height(400.0).show(ui, |ui| match &r.docs {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                muted(ui, format!("Asking {}…", s.label()));
                            });
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(format!("Couldn't list the documents: {e}")).color(theme::error()));
                        }
                        Some(Ok(list)) => {
                            let f = r.filter.to_lowercase();
                            let shown: Vec<&RemoteDoc> = list.iter().filter(|d| f.is_empty() || d.path.to_lowercase().contains(&f)).collect();
                            if shown.is_empty() {
                                muted(ui, if list.is_empty() { "No documents in its project folders yet." } else { "No document matches the search." });
                            }
                            for d in shown {
                                Frame::new().inner_margin(Margin::symmetric(4, 4)).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(RichText::new(&d.name).strong());
                                                if d.project != d.name && !d.project.is_empty() {
                                                    ui.label(RichText::new(format!("in {}", d.project)).color(theme::muted()));
                                                }
                                                if d.git {
                                                    ui.label(RichText::new("git").size(10.5).color(theme::accent()));
                                                }
                                                if d.busy {
                                                    ui.label(RichText::new("Claude is working on it").size(10.5).color(theme::muted()));
                                                }
                                            });
                                            muted(ui, format!("{} · {}", ago(d.modified), size(d.size)));
                                        });
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            if r.opening.as_ref().is_some_and(|(id, _)| *id == d.id) {
                                                ui.spinner();
                                            } else if ui.add_enabled(!busy, egui::Button::new("Open")).on_hover_text(&d.path).clicked() {
                                                pick = Some(d.clone());
                                            }
                                        });
                                    });
                                });
                                ui.separator();
                            }
                        }
                    });
                    muted(ui, format!("Working copies are kept in {}", remote::working_dir(&s.label()).map(|d| d.display().to_string()).unwrap_or_default()));
                }
            }
        });
    if !open {
        app.remote.open = false;
    }
    if let Some(s) = pair {
        app.remote.pair_error = None;
        app.remote.pairing = Some(spawn(Some(ctx), move || {
            let c = Client::connect(&s)?;
            let docs = c.documents()?;
            // the address that answered first, then the others
            let mut urls = vec![c.base.clone()];
            urls.extend(s.urls.iter().filter(|u| **u != c.base).cloned());
            Ok((Server { urls, token: s.token, host: c.host.clone() }, docs))
        }));
    }
    if unpair {
        let _ = remote::save_server(None);
        let r = &mut app.remote;
        r.server = None;
        r.docs = None;
    }
    if reload {
        app.remote.docs = None;
        load_docs(app);
    }
    if let Some(d) = pick {
        open_doc(app, d);
    }
}

fn size(b: u64) -> String {
    if b >= 1 << 20 { format!("{:.1} MB", b as f64 / (1u64 << 20) as f64) } else { format!("{} kB", (b + 1023) / 1024) }
}

fn open_doc(app: &mut App, d: RemoteDoc) {
    let host = app.remote.server.as_ref().map(|s| s.label()).unwrap_or_default();
    let local = remote::id_path(&d.id).and_then(|rel| Some(remote::working_dir(&host)?.join(rel)));
    if local.is_some() && local == app.path {
        // already open: look for changes made over there instead of downloading over it
        app.remote.open = false;
        app.remote.current = local;
        app.remote.polled = None;
        app.remote.dismissed = None;
        app.flash_info(format!("{} is open already; checking {host} for changes", d.name), Some(Duration::from_secs(3)));
        return;
    }
    let id = d.id.clone();
    if let Some(rx) = with_client(app, move |c| remote::open(c, &d), Err) {
        app.remote.opening = Some((id, rx));
    }
}

fn resolve(app: &mut App, f: impl FnOnce(&Client) -> Result<Option<PathBuf>, String> + Send + 'static) {
    app.remote.resolving = with_client(app, f, Err);
}

fn conflict_window(app: &mut App, ctx: &egui::Context) {
    let host = host_name(app);
    let busy = app.remote.resolving.is_some();
    let mut choice = None;
    let c = app.remote.conflict.as_mut().unwrap();
    let name = c.local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    egui::Modal::new(egui::Id::new("remote-conflict")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.label(RichText::new(format!("{name} changed on {host}")).family(theme::medium()).size(15.0));
        ui.add_space(4.0);
        ui.label(format!(
            "Someone changed it on {host} (the phone app, Claude, or another computer) after you opened it here. Your save is kept here and wasn't sent. Which do you want?"
        ));
        ui.add_space(8.0);
        if ui.add_enabled(!busy, egui::Button::new(format!("Reload {host}'s version"))).on_hover_text("Open their copy; the changes made here are dropped").clicked() {
            choice = Some(0);
        }
        if ui.add_enabled(!busy, egui::Button::new("Keep mine")).on_hover_text(format!("Send this copy to {host}, replacing their changes")).clicked() {
            choice = Some(1);
        }
        ui.horizontal(|ui| {
            theme::field_style(ui);
            if ui
                .add_enabled(!busy && !c.copy_name.trim().is_empty(), egui::Button::new("Save mine as a copy:"))
                .on_hover_text(format!("Keep both: this copy goes to {host} as a new document beside theirs"))
                .clicked()
            {
                choice = Some(2);
            }
            ui.add(egui::TextEdit::singleline(&mut c.copy_name).desired_width(200.0));
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if busy {
                ui.spinner();
            } else if ui.button("Decide later").on_hover_text("Keep working here; the next save asks again").clicked() {
                choice = Some(3);
            }
        });
    });
    let Some(choice) = choice else { return };
    let c = app.remote.conflict.as_ref().unwrap();
    let (local, theirs, copy) = (c.local.clone(), c.theirs.clone(), c.copy_name.trim().to_string());
    match choice {
        0 => resolve(app, move |c| remote::reload(c, &local).map(|_| Some(local))),
        1 => {
            let base = theirs.unwrap_or_else(|| "*".into());
            resolve(app, move |c| remote::push(c, &local, Some(&base)).map(|_| None).map_err(|e| e.to_string()))
        }
        2 => resolve(app, move |c| remote::push_copy(c, &local, &copy).map(Some)),
        _ => {}
    }
    app.remote.conflict = None;
}

fn changed_window(app: &mut App, ctx: &egui::Context) {
    let host = host_name(app);
    let Some(p) = app.remote.current.clone() else { return };
    // (the session rehashes after edits; hashing here every frame would be slow for big documents)
    let dirty = app.is_dirty();
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut choice = None;
    egui::Window::new(format!("{name} changed on {host}"))
        .id(egui::Id::new("remote-changed"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 56.0))
        .show(ctx, |ui| {
            ui.set_width(340.0);
            if dirty {
                ui.label(format!("It was changed on {host} (the phone app or another computer). Reloading drops your unsaved changes here; if you save instead you'll be asked which version to keep."));
            } else {
                ui.label(format!("It was changed on {host} (the phone app or another computer). Reload to see the changes."));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(app.remote.resolving.is_none(), egui::Button::new(if dirty { "Reload theirs" } else { "Reload" })).clicked() {
                    choice = Some(true);
                }
                if ui.button("Not now").clicked() {
                    choice = Some(false);
                }
            });
        });
    match choice {
        Some(true) => resolve(app, move |c| remote::reload(c, &p).map(|_| Some(p))),
        Some(false) => app.remote.dismissed = app.remote.changed.clone(),
        None => {}
    }
}

/// How to name a saved file in messages: a working copy by where it is on the other computer.
pub fn shown_path(p: &Path) -> String {
    match remote::link_for(p) {
        Some(l) if !l.remote_path.is_empty() => format!("{} on {}", l.remote_path, l.host),
        _ => p.display().to_string(),
    }
}

/// When the save of a remote document last reached it, for the status line: e.g. "netdash", or
/// "netdash (offline)". None for an ordinary document.
pub fn status(app: &App) -> Option<String> {
    let p = app.remote.current.as_ref()?;
    let host = app.remote.server.as_ref().map(|s| s.label()).or_else(|| remote::link_for(p).map(|l| l.host))?;
    Some(if app.remote.offline.is_some() { format!("on {host} (can't reach it)") } else { format!("on {host}") })
}
