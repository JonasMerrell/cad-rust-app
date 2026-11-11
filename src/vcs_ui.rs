//! The Version control window: track a document with git, commit (by hand or on every save),
//! browse its history, see what each commit changed, open an earlier version, and publish the
//! project to GitHub, and keep it in sync there (pull on open, push after each commit, a choice
//! when both sides changed). The git work itself is in `vcs.rs`; the GitHub account and Open
//! from GitHub windows are in `github_ui.rs`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, UNIX_EPOCH};

use egui::{Frame, Margin, RichText, Ui};

use crate::app::{App, ToastKind};
use crate::icons;
use crate::theme;
use crate::vcs::{self, Commit, GhUser, Identity, Status, TokenSource};
use crate::workspace::Workspace;

/// What a background job (network calls) came back with.
pub(crate) enum Done {
    Published(String),
    Account(Option<(GhUser, TokenSource)>),
    /// A sync of the document's repository, and the document.
    Synced(vcs::Synced, PathBuf),
    /// A divergence settled (message, document, whether the files changed).
    Resolved(String, PathBuf, bool),
}

#[derive(Default)]
pub struct VcsUi {
    pub open: bool,
    /// The document the status below belongs to (None: an unsaved document).
    shown_for: Option<Option<PathBuf>>,
    pub status: Option<Status>,
    history: Vec<Commit>,
    /// The installed git's version, once looked up (None inside: no git).
    pub(crate) git: Option<Option<String>>,
    message: String,
    all_files: bool,
    /// The next save commits even with commit-on-save off (the Commit button saved first).
    commit_next_save: bool,
    /// The selected commit, and what it changed (or why that couldn't be read).
    selected: Option<(String, Result<Vec<String>, String>)>,
    /// The selected commit compared with the document as it is now.
    vs_now: Option<(String, Result<Vec<String>, String>)>,
    remote_url: String,
    /// The GitHub repository name to publish as, and the document it was suggested for.
    repo: String,
    repo_for: Option<PathBuf>,
    private: bool,
    pub(crate) account: Option<(GhUser, TokenSource)>,
    pub(crate) account_checked: bool,
    pub(crate) account_rx: Option<Receiver<Done>>,
    /// A push, pull or publish under way: what it is, and where its result arrives.
    busy: Option<(String, Receiver<Result<Done, String>>)>,
    /// The last result, shown in the window (text, is an error).
    note: Option<(String, bool)>,
    /// The New project window: name, parent folder, track with git.
    new_project: Option<(String, String, bool)>,
    /// The document last synced when opened (so each document syncs once when it opens).
    synced_for: Option<PathBuf>,
    /// Sync once the job under way is done (a save committed while it ran).
    sync_pending: bool,
    /// A document saved while a job ran: commit it once that is done.
    commit_after_busy: Option<PathBuf>,
    /// This copy and the remote both have commits that couldn't be merged.
    diverged: Option<vcs::Divergence>,
    /// The last sync failed on its own (opening, after a save): said once, not every time.
    auto_failed: bool,
}

// ------------------------------------------------------------ projects

/// Open the New project window.
pub fn new_project(app: &mut App) {
    app.vcs.new_project = Some((String::new(), crate::project::default_dir().display().to_string(), app.vcs.git.clone().flatten().is_some() || vcs::version().is_some()));
}

/// Pick a project ZIP, unpack it into the projects folder and open its document.
pub fn import_project(app: &mut App) {
    // opening a ZIP unpacks it into the projects folder (see `App::open_path`)
    app.with_open_path("Import project", "Project archive (ZIP)", &["zip", "ZIP"], |a, p| a.guard(crate::session::Pending::OpenPath(p)));
}

/// Save the document's project folder (git history included) as a ZIP.
pub fn export_project(app: &mut App) {
    let Some(doc) = app.path.clone() else {
        app.flash("Save the document first, then export its project", ToastKind::Info);
        return;
    };
    if app.is_dirty() {
        app.save(false);
    }
    let name = crate::project::folder_of(&doc)
        .and_then(|d| d.file_name().map(|s| s.to_string_lossy().into_owned()))
        .or_else(|| doc.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".into());
    let mc_warn = crate::mcmaster::share_warning(&crate::mcmaster::parts_in(&app.ws));
    app.with_save_path("Export project", "Project archive (ZIP)", &["zip"], &format!("{name}.zip"), move |a, p| match {
        // the built geometry goes in too (see `build_cache`), so the project opens without a rebuild
        crate::build_cache::saved(a, &doc);
        crate::build_cache::wait_written(Duration::from_secs(60));
        crate::project::export(&doc, &p, true)
    } {
        Ok(n) => match &mc_warn {
            Some(w) => a.flash(format!("Exported the project ({n} file{}) to {}. {w}", if n == 1 { "" } else { "s" }, p.display()), ToastKind::Info),
            None => a.flash_info(format!("Exported the project ({n} file{}) to {}", if n == 1 { "" } else { "s" }, p.display()), Some(Duration::from_secs(4))),
        },
        Err(e) => a.flash(format!("Could not export the project: {e}"), ToastKind::Error),
    });
}

fn new_project_window(app: &mut App, ctx: &egui::Context) {
    let Some((name, dir, git)) = app.vcs.new_project.as_mut() else { return };
    let mut open = true;
    let mut create = false;
    egui::Window::new("New project").open(&mut open).default_pos(ctx.content_rect().center() - egui::vec2(190.0, 100.0)).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.set_width(380.0);
        egui::Grid::new("new-project").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(name).hint_text("e.g. Robot arm").desired_width(280.0));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                create = true;
            }
            if name.is_empty() && ui.memory(|m| m.focused().is_none()) {
                r.request_focus();
            }
            ui.end_row();
            ui.label("Folder");
            ui.add(egui::TextEdit::singleline(dir).desired_width(280.0));
            ui.end_row();
        });
        ui.checkbox(git, "Track changes with git (every save is committed)");
        let shown = crate::project::clean_name(name);
        let made = match crate::project::expand_dir(dir) {
            Ok(d) => format!("Makes {}", d.join(&shown).join(format!("{shown}.json")).display()),
            Err(e) => e,
        };
        ui.label(RichText::new(made).color(theme::muted()).size(11.5));
        ui.add_space(4.0);
        if ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Create project")).clicked() {
            create = true;
        }
    });
    if !open {
        app.vcs.new_project = None;
    }
    if create {
        let Some((name, dir, git)) = app.vcs.new_project.take() else { return };
        if name.trim().is_empty() {
            app.vcs.new_project = Some((name, dir, git));
            return;
        }
        match crate::project::expand_dir(&dir).and_then(|d| crate::project::create(&d, &name)) {
            Ok(doc) => app.guard(crate::session::Pending::NewProject(doc, git)),
            Err(e) => app.flash(e, ToastKind::Error),
        }
    }
}

pub fn toggle(app: &mut App) {
    app.vcs.open = !app.vcs.open;
    if app.vcs.open {
        refresh(app);
    }
}

/// Read the document's repository status and history again.
pub fn refresh(app: &mut App) {
    let v = &mut app.vcs;
    let moved = v.repo_for != app.path;
    v.repo_for = app.path.clone();
    v.shown_for = Some(app.path.clone());
    v.status = app.path.as_deref().and_then(vcs::status);
    v.history = v.status.as_ref().map(|s| vcs::history(&s.root, &s.rel, 300)).unwrap_or_default();
    if let Some((h, _)) = &v.selected {
        if !v.history.iter().any(|c| &c.hash == h) {
            v.selected = None;
            v.vs_now = None;
        }
    }
    if let Some(s) = &v.status {
        if let Some(r) = &s.remote {
            v.remote_url = r.clone();
        }
    }
    if moved || v.repo.is_empty() {
        v.repo = vcs::repo_name(&app.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    }
}

fn identity(app: &App) -> Identity {
    Identity::guess(app.vcs.account.as_ref().map(|(u, _)| u))
}

/// After a save to `p`: commit it when the repository commits on save (or the Commit button
/// asked for it), then sync it when it syncs. Returns a note for the "Saved" message.
pub fn after_save(app: &mut App, p: &std::path::Path) -> String {
    if app.vcs.busy.is_some() {
        // a sync may be merging into the folder right now: commit once it is done
        app.vcs.commit_after_busy = Some(p.to_path_buf());
        return " · commits once the sync under way is done".into();
    }
    commit_saved(app, p)
}

fn commit_saved(app: &mut App, p: &std::path::Path) -> String {
    let forced = std::mem::take(&mut app.vcs.commit_next_save);
    let Some(s) = vcs::status(p) else {
        app.vcs.shown_for = None;
        return String::new();
    };
    if !(s.autocommit || forced) {
        app.vcs.shown_for = None;
        return String::new();
    }
    let typed = std::mem::take(&mut app.vcs.message);
    let msg = if typed.trim().is_empty() { vcs::save_message(&s.root, &s.rel, &app.ws) } else { typed };
    let who = identity(app);
    let note = match vcs::commit(&s.root, &s.rel, &msg, app.vcs.all_files, &who) {
        Ok(Some(c)) => {
            if s.sync {
                app.vcs.sync_pending = true;
            }
            format!(" · committed “{}”{}", c.subject, if s.sync { ", syncing" } else { "" })
        }
        Ok(None) => String::new(),
        Err(e) => {
            app.vcs.note = Some((format!("Could not commit: {e}"), true));
            " · the commit failed (see Version control)".into()
        }
    };
    app.vcs.shown_for = None; // refreshed next frame
    note
}

/// Sync the open document's repository in the background: commit its saved changes first
/// (when it commits on save), then pull, merge and push (see `vcs::sync`).
pub fn sync_now(app: &mut App) {
    let Some(path) = app.path.clone() else { return };
    if app.vcs.busy.is_some() {
        app.vcs.sync_pending = true;
        return;
    }
    let Some(s) = vcs::status(&path) else { return };
    if s.remote.is_none() {
        return;
    }
    if s.changed && s.autocommit && !app.is_dirty() {
        let msg = vcs::save_message(&s.root, &s.rel, &app.ws);
        let who = identity(app);
        if let Err(e) = vcs::commit(&s.root, &s.rel, &msg, false, &who) {
            app.vcs.note = Some((format!("Could not commit: {e}"), true));
            return;
        }
    }
    let who = identity(app);
    let root = s.root.clone();
    run(app, "Syncing with GitHub…", move || {
        let token = vcs::token().map(|(t, _)| t);
        vcs::sync(&root, token.as_deref(), &who).map(|r| Done::Synced(r, path))
    });
}

/// Runs every frame: keeps the status in step with the open document, takes in background
/// results and shows the window.
pub fn frame(app: &mut App, ctx: &egui::Context) {
    if app.vcs.shown_for.as_ref() != Some(&app.path) {
        refresh(app);
    }
    poll(app);
    if app.vcs.busy.is_some() {
        ctx.request_repaint_after(Duration::from_millis(250));
    }
    // a document opens: bring in what was pushed from elsewhere
    if app.path.is_some() && app.vcs.synced_for != app.path {
        app.vcs.synced_for = app.path.clone();
        app.vcs.diverged = None;
        app.vcs.auto_failed = false;
        if app.vcs.status.as_ref().is_some_and(|s| s.sync) && !app.is_dirty() {
            sync_now(app);
        }
    }
    if app.vcs.busy.is_none() {
        if let Some(p) = app.vcs.commit_after_busy.take() {
            let note = commit_saved(app, &p);
            if !note.is_empty() {
                app.flash_info(format!("Saved {}{note}", p.display()), Some(Duration::from_secs(3)));
            }
        }
        if std::mem::take(&mut app.vcs.sync_pending) {
            sync_now(app);
        }
    }
    crate::github_ui::frame(app, ctx);
    // not in the document's thumbnail
    if app.vcs.open && !crate::home::hide_hover(app) {
        window(app, ctx);
    }
    new_project_window(app, ctx);
}

fn poll(app: &mut App) {
    if let Some(rx) = &app.vcs.account_rx {
        if let Ok(Done::Account(a)) = rx.try_recv() {
            app.vcs.account = a;
            app.vcs.account_rx = None;
        }
    }
    let Some((what, rx)) = &app.vcs.busy else { return };
    let Ok(r) = rx.try_recv() else { return };
    let what = what.clone();
    app.vcs.busy = None;
    match r {
        Ok(Done::Published(url)) => app.vcs.note = Some((format!("Published to {}", vcs::short_remote(&url)), false)),
        Ok(Done::Account(_)) => {}
        Ok(Done::Synced(r, doc)) => {
            app.vcs.auto_failed = false;
            match r {
                vcs::Synced::UpToDate => app.vcs.note = Some(("Up to date with GitHub".into(), false)),
                vcs::Synced::Pushed(m) => app.vcs.note = Some((m, false)),
                vcs::Synced::Pulled(m) | vcs::Synced::Merged(m) => {
                    app.vcs.diverged = None;
                    app.vcs.note = Some((m.clone(), false));
                    reload(app, doc, m);
                }
                vcs::Synced::Diverged(d) => {
                    app.vcs.diverged = Some(d);
                    app.vcs.note = None;
                    app.vcs.open = true;
                    app.flash("This project changed on GitHub and here in ways that can't be combined: pick a version in Version control", ToastKind::Info);
                }
            }
        }
        Ok(Done::Resolved(m, doc, changed)) => {
            app.vcs.diverged = None;
            app.vcs.note = Some((m.clone(), false));
            if changed {
                reload(app, doc, m);
            }
        }
        Err(e) => {
            if what.starts_with("Syncing") && !app.vcs.open {
                // said once as a toast (offline, say), then only in the window
                if !std::mem::replace(&mut app.vcs.auto_failed, true) {
                    app.flash(format!("Could not sync with GitHub: {e}"), ToastKind::Info);
                }
            }
            app.vcs.note = Some((e, true));
        }
    }
    refresh(app);
}

/// The document on disk is newer: open it again if it is still the one open, asking first about
/// changes made in the meantime.
fn reload(app: &mut App, doc: PathBuf, m: String) {
    if app.path.as_ref() == Some(&doc) && doc.exists() {
        app.guard(crate::session::Pending::OpenPath(doc));
        app.flash_info(m, Some(Duration::from_secs(3)));
    }
}

/// Look up the GitHub account in the background (the token may come from `gh`, and the user
/// from the API).
pub(crate) fn check_account(app: &mut App) {
    app.vcs.account_checked = true;
    let (tx, rx) = channel();
    app.vcs.account_rx = Some(rx);
    std::thread::spawn(move || {
        let a = vcs::token().and_then(|(t, src)| vcs::gh_user(&t).ok().map(|u| (u, src)));
        let _ = tx.send(Done::Account(a));
    });
}

fn run(app: &mut App, what: &str, job: impl FnOnce() -> Result<Done, String> + Send + 'static) {
    let (tx, rx) = channel();
    app.vcs.note = None;
    app.vcs.busy = Some((what.to_string(), rx));
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
}

/// The branch badge in the header: the branch when the document is tracked, else a way in.
pub fn badge(app: &mut App, ui: &mut Ui) {
    let (text, tip) = match &app.vcs.status {
        Some(s) => {
            let mut t = s.branch.clone();
            if s.ahead > 0 {
                t.push_str(&format!(" ↑{}", s.ahead));
            }
            if s.behind > 0 {
                t.push_str(&format!(" ↓{}", s.behind));
            }
            let tip = match &s.remote {
                Some(r) => format!("Tracked with git, on {}: version control", vcs::short_remote(r)),
                None => "Tracked with git: version control".into(),
            };
            (t, tip)
        }
        None => ("Main".into(), "Version control: track this document's changes with git".into()),
    };
    let tracked = app.vcs.status.is_some();
    let r = Frame::new()
        .fill(theme::panel_2())
        .corner_radius(4)
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if tracked {
                ui.add(icons::image("branch", 11.0).tint(theme::muted()));
            }
            ui.label(RichText::new(text).size(11.0).color(theme::muted()));
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tip);
    if r.clicked() {
        toggle(app);
    }
}

fn when(t: u64) -> String {
    crate::home::when(UNIX_EPOCH + Duration::from_secs(t))
}

fn heading(ui: &mut Ui, t: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(t.to_uppercase()).family(theme::medium()).color(theme::muted()).size(10.5));
    ui.add_space(2.0);
}

enum Act {
    Track,
    TrackInFolder,
    SaveFirst,
    Commit,
    Autocommit(bool),
    Select(String),
    CompareNow(String),
    OpenCommit(String),
    Sync,
    SetSync(bool),
    Keep(bool),
    Publish,
    Connect,
    Account,
}

fn window(app: &mut App, ctx: &egui::Context) {
    if app.vcs.git.is_none() {
        app.vcs.git = Some(vcs::version());
    }
    if !app.vcs.account_checked {
        check_account(app);
    }
    let mut open = true;
    let mut act = None;
    let dirty = app.is_dirty();
    let mc_warn = crate::mcmaster::share_warning(&crate::mcmaster::parts_in(&app.ws));
    egui::Window::new("Version control")
        .open(&mut open)
        .default_size([440.0, 640.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(220.0, 280.0))
        .constrain(true)
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| {
            let v = &mut app.vcs;
            if v.git.as_ref().is_some_and(|g| g.is_none()) {
                ui.label("Version control uses git, which isn't installed.");
                ui.hyperlink_to("Download git", "https://git-scm.com/downloads");
                return;
            }
            let Some(path) = app.path.clone() else {
                ui.label("Save the document first: its folder then becomes a git repository that records each saved version.");
                ui.add_space(6.0);
                if ui.button("Save as…").clicked() {
                    act = Some(Act::SaveFirst);
                }
                return;
            };
            let Some(s) = v.status.clone() else {
                ui.label(RichText::new("This document isn't tracked yet.").strong());
                ui.label(
                    RichText::new(format!(
                        "Tracking makes its folder ({}) a git repository. Every save is then committed, so you can see and open any earlier version, and push the project to GitHub.",
                        path.parent().map(|p| p.display().to_string()).unwrap_or_default()
                    ))
                    .color(theme::muted()),
                );
                ui.add_space(8.0);
                let busy_folder = vcs::folder_neighbours(&path) >= 8;
                if busy_folder {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new("The folder holds other files too, so a folder of its own keeps the project's history to itself.").color(theme::muted()));
                    if ui.add(egui::Button::image_and_text(icons::image("folder", 15.0).tint(theme::text()), format!("Move into a folder “{name}” and track it"))).clicked() {
                        act = Some(Act::TrackInFolder);
                    }
                }
                if ui.add(egui::Button::image_and_text(icons::image("branch", 15.0).tint(theme::text()), if busy_folder { "Track it where it is" } else { "Track changes with git" })).clicked() {
                    act = Some(Act::Track);
                }
                note_ui(ui, &v.note);
                return;
            };

            // where the document stands
            ui.horizontal_wrapped(|ui| {
                ui.add(icons::image("branch", 14.0).tint(theme::accent()));
                ui.label(RichText::new(&s.branch).strong());
                ui.label(RichText::new(format!("· {}", s.root.display())).color(theme::muted()).size(11.5));
            });
            let state = if dirty {
                "Unsaved changes: saving commits them".to_string()
            } else if s.changed {
                "Saved changes that aren't committed".to_string()
            } else {
                "Everything is committed".to_string()
            };
            let state = if !s.autocommit && dirty { "Unsaved changes".to_string() } else { state };
            ui.label(RichText::new(state).color(theme::muted()));

            heading(ui, "Commit");
            ui.add(egui::TextEdit::multiline(&mut v.message).hint_text("Message (empty: a list of what changed)").desired_rows(2).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                let can = dirty || s.changed || (v.all_files && !s.others.is_empty());
                if ui.add_enabled(can && v.busy.is_none(), egui::Button::new(if dirty { "Save and commit" } else { "Commit" })).clicked() {
                    act = Some(Act::Commit);
                }
                let mut auto = s.autocommit;
                if ui.checkbox(&mut auto, "Commit on every save").changed() {
                    act = Some(Act::Autocommit(auto));
                }
            });
            if !s.others.is_empty() {
                let list = s.others.iter().take(12).cloned().collect::<Vec<_>>().join("\n");
                ui.checkbox(&mut v.all_files, format!("Also commit the folder's other changed files ({})", s.others.len())).on_hover_text(list);
            }

            heading(ui, &format!("History ({})", v.history.len()));
            egui::ScrollArea::vertical().id_salt("vcs-history").auto_shrink([false, true]).max_height(260.0).show(ui, |ui| {
                if v.history.is_empty() {
                    ui.label(RichText::new("No commits yet").color(theme::muted()));
                }
                for (i, c) in v.history.iter().enumerate() {
                    let sel = v.selected.as_ref().is_some_and(|(h, _)| *h == c.hash);
                    let r = ui
                        .add(egui::Button::selectable(
                            sel,
                            two_lines(ui, &c.subject, &format!("{} · {} · {}", c.short, c.author, when(c.time))),
                        ))
                        .on_hover_text(&c.hash);
                    if r.clicked() {
                        act = Some(Act::Select(c.hash.clone()));
                    }
                    if sel {
                        Frame::new().inner_margin(Margin { left: 12, right: 4, top: 2, bottom: 6 }).show(ui, |ui| {
                            match v.selected.as_ref().map(|(_, ch)| ch) {
                                Some(Ok(ch)) if ch.is_empty() => {
                                    ui.label(RichText::new(if i + 1 == v.history.len() { "The first commit" } else { "No changes to the model (settings or the open tab)" }).color(theme::muted()).size(11.5));
                                }
                                Some(Ok(ch)) => {
                                    for l in ch {
                                        ui.label(RichText::new(format!("• {l}")).size(11.5));
                                    }
                                }
                                Some(Err(e)) => {
                                    ui.label(RichText::new(e).color(theme::error()).size(11.5));
                                }
                                None => {}
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button("Compare with now").clicked() {
                                    act = Some(Act::CompareNow(c.hash.clone()));
                                }
                                if ui.small_button("Open this version").on_hover_text("Show the document as it was then (Ctrl+Z comes back; saving keeps it)").clicked() {
                                    act = Some(Act::OpenCommit(c.hash.clone()));
                                }
                            });
                            if let Some((h, r)) = &v.vs_now {
                                if *h == c.hash {
                                    ui.label(RichText::new("Since then:").size(11.5).strong());
                                    match r {
                                        Ok(ch) if ch.is_empty() => {
                                            ui.label(RichText::new("No changes to the model").color(theme::muted()).size(11.5));
                                        }
                                        Ok(ch) => {
                                            for l in ch {
                                                ui.label(RichText::new(format!("• {l}")).size(11.5));
                                            }
                                        }
                                        Err(e) => {
                                            ui.label(RichText::new(e).color(theme::error()).size(11.5));
                                        }
                                    }
                                }
                            }
                        });
                    }
                }
            });

            heading(ui, "GitHub");
            if let Some(w) = &mc_warn {
                ui.label(RichText::new(w).color(theme::error()).size(11.5));
            }
            match &s.remote {
                Some(r) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(vcs::short_remote(r)).strong());
                        if let Some(web) = vcs::web_url(r) {
                            if ui.small_button("Open on GitHub").clicked() {
                                crate::printing::open_url(&web);
                            }
                        }
                    });
                    let idle = v.busy.is_none();
                    match &v.diverged {
                        Some(d) => {
                            Frame::new().fill(theme::panel_2()).corner_radius(4).inner_margin(Margin::same(8)).show(ui, |ui| {
                                ui.label(RichText::new("This computer and GitHub both have new commits, and they can't be combined automatically.").strong());
                                let list = |ui: &mut Ui, title: &str, cs: &[Commit]| {
                                    ui.label(RichText::new(title).size(11.5).color(theme::muted()));
                                    for c in cs.iter().take(4) {
                                        ui.label(RichText::new(format!("• {} · {} · {}", c.subject, c.author, when(c.time))).size(11.5));
                                    }
                                    if cs.len() > 4 {
                                        ui.label(RichText::new(format!("  and {} more", cs.len() - 4)).size(11.5).color(theme::muted()));
                                    }
                                };
                                list(ui, "Only on GitHub:", &d.theirs[..]);
                                list(ui, "Only on this computer:", &d.mine[..]);
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if ui.add_enabled(idle && !dirty, egui::Button::new("Keep this computer's")).on_hover_text("Push this computer's version; GitHub's stays in History").clicked() {
                                        act = Some(Act::Keep(true));
                                    }
                                    if ui.add_enabled(idle && !dirty, egui::Button::new("Keep GitHub's")).on_hover_text("Open GitHub's version; this computer's stays in History").clicked() {
                                        act = Some(Act::Keep(false));
                                    }
                                });
                                ui.label(RichText::new("Either way both versions stay in the history, so the other one can still be opened from there.").size(11.0).color(theme::muted()));
                            });
                        }
                        None => {
                            ui.horizontal(|ui| {
                                let mut counts = vec![];
                                if !s.upstream && s.has_commits {
                                    counts.push("not pushed yet".to_string());
                                }
                                if s.ahead > 0 {
                                    counts.push(format!("↑{}", s.ahead));
                                }
                                if s.behind > 0 {
                                    counts.push(format!("↓{}", s.behind));
                                }
                                let label = if counts.is_empty() { "Sync now".to_string() } else { format!("Sync now ({})", counts.join(" ")) };
                                if ui.add_enabled(idle, egui::Button::new(label)).on_hover_text("Pull what was pushed from elsewhere and push this computer's commits").clicked() {
                                    act = Some(Act::Sync);
                                }
                                let mut auto = s.sync;
                                if ui.checkbox(&mut auto, "Sync automatically").on_hover_text("Pull when the document opens and push after each commit").changed() {
                                    act = Some(Act::SetSync(auto));
                                }
                            });
                        }
                    }
                }
                None => {
                    ui.label(RichText::new("Not on GitHub yet.").color(theme::muted()));
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut v.repo).desired_width(170.0));
                        ui.checkbox(&mut v.private, "Private");
                    });
                    let signed_in = v.account.is_some();
                    let b = ui.add_enabled(signed_in && v.busy.is_none() && s.has_commits && !v.repo.trim().is_empty(), egui::Button::new("Publish to GitHub"));
                    if b.on_disabled_hover_text("Sign in to GitHub first (Sign in… below)").clicked() {
                        act = Some(Act::Publish);
                    }
                    egui::CollapsingHeader::new(RichText::new("Or use an existing repository").size(12.0)).id_salt("vcs-existing").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut v.remote_url).hint_text("https://github.com/you/project.git").desired_width(250.0));
                            if ui.button("Connect").clicked() {
                                act = Some(Act::Connect);
                            }
                        });
                    });
                }
            }
            if let Some((what, _)) = &v.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(what).color(theme::muted()));
                });
            }
            note_ui(ui, &v.note);

            // the account
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let who = match &v.account {
                    Some((u, TokenSource::Saved)) => format!("Signed in to GitHub as {}", u.login),
                    Some((u, TokenSource::Env)) => format!("Signed in to GitHub as {} (token from the environment)", u.login),
                    Some((u, TokenSource::GhCli)) => format!("Signed in to GitHub as {} (from the GitHub CLI)", u.login),
                    None if v.account_rx.is_some() => "Checking the GitHub sign-in…".into(),
                    None => "Not signed in to GitHub: Push and Pull use git's own sign-in".into(),
                };
                ui.label(RichText::new(who).color(theme::muted()).size(11.5));
                if ui.small_button(if v.account.is_some() { "Account…" } else { "Sign in…" }).clicked() {
                    act = Some(Act::Account);
                }
            });
        });
    if !open {
        app.vcs.open = false;
    }
    if let Some(a) = act {
        act_on(app, a);
    }
}

/// History row text: the subject, then smaller muted details.
fn two_lines(ui: &Ui, subject: &str, details: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let style = ui.style();
    RichText::new(format!("{subject}\n")).size(12.5).color(theme::text()).append_to(&mut job, style, egui::FontSelection::Default, egui::Align::LEFT);
    RichText::new(details).size(11.0).color(theme::muted()).append_to(&mut job, style, egui::FontSelection::Default, egui::Align::LEFT);
    job
}

fn note_ui(ui: &mut Ui, note: &Option<(String, bool)>) {
    if let Some((t, err)) = note {
        ui.label(RichText::new(t).color(if *err { theme::error() } else { theme::ok_green() }).size(11.5));
    }
}

/// The workspace in a commit.
fn ws_at(s: &Status, hash: &str) -> Result<Workspace, String> {
    vcs::file_at(&s.root, hash, &s.rel).and_then(|t| Workspace::from_json(&t))
}

fn act_on(app: &mut App, a: Act) {
    let Some(path) = app.path.clone() else {
        if let Act::SaveFirst = a {
            app.save(true);
        }
        return;
    };
    let token = || vcs::token().map(|(t, _)| t);
    match a {
        Act::SaveFirst => app.save(true),
        Act::Track | Act::TrackInFolder => {
            if app.is_dirty() {
                app.save(false);
            }
            let path = if matches!(a, Act::TrackInFolder) {
                match vcs::move_into_own_folder(&path) {
                    Ok(p) => {
                        crate::home::forget(app, &path);
                        crate::home::touch(app, &p);
                        app.path = Some(p.clone());
                        p
                    }
                    Err(e) => {
                        app.vcs.note = Some((e, true));
                        return;
                    }
                }
            } else {
                path
            };
            let who = identity(app);
            match vcs::init(&path, &who) {
                Ok(_) => app.vcs.note = Some(("Tracking changes: every save is now committed".into(), false)),
                Err(e) => app.vcs.note = Some((format!("Could not start tracking: {e}"), true)),
            }
            refresh(app);
        }
        Act::Commit => {
            if app.is_dirty() {
                app.vcs.commit_next_save = true;
                app.save(false);
            } else if let Some(s) = app.vcs.status.clone() {
                let msg = std::mem::take(&mut app.vcs.message);
                let msg = if msg.trim().is_empty() { vcs::save_message(&s.root, &s.rel, &app.ws) } else { msg };
                let who = identity(app);
                app.vcs.note = match vcs::commit(&s.root, &s.rel, &msg, app.vcs.all_files, &who) {
                    Ok(Some(c)) => Some((format!("Committed “{}”", c.subject), false)),
                    Ok(None) => Some(("Nothing to commit".into(), false)),
                    Err(e) => Some((format!("Could not commit: {e}"), true)),
                };
            }
            refresh(app);
        }
        Act::Autocommit(on) => {
            if let Some(s) = &app.vcs.status {
                if let Err(e) = vcs::set_autocommit(&s.root, on) {
                    app.vcs.note = Some((e, true));
                }
            }
            refresh(app);
        }
        Act::Select(h) => {
            app.vcs.vs_now = None;
            if app.vcs.selected.as_ref().is_some_and(|(s, _)| *s == h) {
                app.vcs.selected = None;
                return;
            }
            let Some(s) = app.vcs.status.clone() else { return };
            // what this commit changed: against the commit before it
            let changes = ws_at(&s, &h).map(|new| match ws_at(&s, &format!("{h}^")) {
                Ok(old) => vcs::compare(&old, &new),
                Err(_) => vec![],
            });
            app.vcs.selected = Some((h, changes));
        }
        Act::CompareNow(h) => {
            let Some(s) = app.vcs.status.clone() else { return };
            app.sync_ws();
            let r = ws_at(&s, &h).map(|old| vcs::compare(&old, &app.ws));
            app.vcs.vs_now = Some((h, r));
        }
        Act::OpenCommit(h) => {
            app.vcs.vs_now = None;
            let Some(s) = app.vcs.status.clone() else { return };
            match ws_at(&s, &h) {
                Ok(mut ws) => {
                    let c = app.vcs.history.iter().find(|c| c.hash == h).cloned();
                    let (short, subject, t) = c.map(|c| (c.short, c.subject, c.time)).unwrap_or_default();
                    // the document's own named versions stay as they are now
                    ws.versions = app.ws.versions.clone();
                    app.replace_workspace(ws, format!("open commit {short}"));
                    app.flash_info(format!("Opened {short} “{subject}” from {} · Ctrl+Z goes back, saving keeps it", when(t).to_lowercase()), Some(Duration::from_secs(5)));
                }
                Err(e) => app.flash(format!("Could not open that version: {e}"), ToastKind::Error),
            }
        }
        Act::Sync => {
            if app.is_dirty() {
                // saving commits, and the commit syncs
                app.vcs.commit_next_save = true;
                app.vcs.sync_pending = true;
                app.save(false);
            } else {
                sync_now(app);
            }
        }
        Act::SetSync(on) => {
            if let Some(s) = &app.vcs.status {
                if let Err(e) = vcs::set_sync(&s.root, on) {
                    app.vcs.note = Some((e, true));
                }
            }
            refresh(app);
            if on {
                sync_now(app);
            }
        }
        Act::Keep(mine) => {
            let Some(root) = app.vcs.status.as_ref().map(|s| s.root.clone()) else { return };
            let who = identity(app);
            run(app, if mine { "Pushing this computer's version…" } else { "Taking GitHub's version…" }, move || {
                vcs::resolve(&root, token().as_deref(), &who, mine).map(|m| Done::Resolved(m, path, !mine))
            });
        }
        Act::Publish => {
            let Some(root) = app.vcs.status.as_ref().map(|s| s.root.clone()) else { return };
            let (name, private) = (app.vcs.repo.trim().to_string(), app.vcs.private);
            run(app, "Publishing to GitHub…", move || {
                let t = token().ok_or("sign in to GitHub first")?;
                vcs::publish(&root, &t, &name, private).map(Done::Published)
            });
        }
        Act::Connect => {
            if let Some(s) = app.vcs.status.clone() {
                app.vcs.note = match vcs::set_remote(&s.root, &app.vcs.remote_url) {
                    Ok(()) => {
                        let _ = vcs::set_sync(&s.root, true);
                        app.vcs.synced_for = None; // syncs next frame
                        Some(("Connected: it syncs there from now on".into(), false))
                    }
                    Err(e) => Some((e, true)),
                };
            }
            refresh(app);
        }
        Act::Account => crate::github_ui::open_account(app),
    }
}
