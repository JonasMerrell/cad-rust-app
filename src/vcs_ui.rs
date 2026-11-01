//! The Version control window: track a document with git, commit (by hand or on every save),
//! browse its history, see what each commit changed, open an earlier version, and publish the
//! project to GitHub, push and pull. The git work itself is in `vcs.rs`.

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
enum Done {
    Note(String),
    /// What happened, and the document the pull was for.
    Pulled(String, PathBuf),
    Published(String),
    Account(Option<(GhUser, TokenSource)>),
}

#[derive(Default)]
pub struct VcsUi {
    pub open: bool,
    /// The document the status below belongs to (None: an unsaved document).
    shown_for: Option<Option<PathBuf>>,
    pub status: Option<Status>,
    history: Vec<Commit>,
    /// The installed git's version, once looked up (None inside: no git).
    git: Option<Option<String>>,
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
    token_input: String,
    account: Option<(GhUser, TokenSource)>,
    account_checked: bool,
    account_rx: Option<Receiver<Done>>,
    /// A push, pull or publish under way: what it is, and where its result arrives.
    busy: Option<(String, Receiver<Result<Done, String>>)>,
    /// The last result, shown in the window (text, is an error).
    note: Option<(String, bool)>,
    /// The New project window: name, parent folder, track with git.
    new_project: Option<(String, String, bool)>,
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
    app.with_save_path("Export project", "Project archive (ZIP)", &["zip"], &format!("{name}.zip"), move |a, p| match crate::project::export(&doc, &p, true) {
        Ok(n) => a.flash_info(format!("Exported the project ({n} file{}) to {}", if n == 1 { "" } else { "s" }, p.display()), Some(Duration::from_secs(4))),
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
/// asked for it). Returns a note for the "Saved" message.
pub fn after_save(app: &mut App, p: &std::path::Path) -> String {
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
        Ok(Some(c)) => format!(" · committed “{}”", c.subject),
        Ok(None) => String::new(),
        Err(e) => {
            app.vcs.note = Some((format!("Could not commit: {e}"), true));
            " · the commit failed (see Version control)".into()
        }
    };
    app.vcs.shown_for = None; // refreshed next frame
    note
}

/// Runs every frame: keeps the status in step with the open document, takes in background
/// results and shows the window.
pub fn frame(app: &mut App, ctx: &egui::Context) {
    if app.vcs.shown_for.as_ref() != Some(&app.path) {
        refresh(app);
    }
    poll(app);
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
    let Some((_, rx)) = &app.vcs.busy else { return };
    let Ok(r) = rx.try_recv() else { return };
    app.vcs.busy = None;
    match r {
        Ok(Done::Note(m)) => app.vcs.note = Some((m, false)),
        Ok(Done::Published(url)) => app.vcs.note = Some((format!("Published to {}", vcs::short_remote(&url)), false)),
        Ok(Done::Pulled(m, doc)) => {
            app.vcs.note = Some((m.clone(), false));
            // the document on disk is newer now: open it again if it is still the one open, asking
            // first about changes made while the pull ran
            if m != "Already up to date" && app.path.as_ref() == Some(&doc) {
                app.guard(crate::session::Pending::OpenPath(doc));
                app.flash_info(m, Some(Duration::from_secs(3)));
            }
        }
        Ok(Done::Account(_)) => {}
        Err(e) => app.vcs.note = Some((e, true)),
    }
    refresh(app);
}

/// Look up the GitHub account in the background (the token may come from `gh`, and the user
/// from the API).
fn check_account(app: &mut App) {
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
    Push,
    Pull,
    Publish,
    Connect,
    SignIn,
    SignOut,
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
                    ui.horizontal(|ui| {
                        let ahead = if !s.upstream { " (not pushed yet)".to_string() } else if s.ahead > 0 { format!(" ({} commit{})", s.ahead, if s.ahead == 1 { "" } else { "s" }) } else { String::new() };
                        let idle = v.busy.is_none() && s.has_commits;
                        if ui.add_enabled(idle, egui::Button::new(format!("Push{ahead}"))).on_hover_text("Send the commits to the remote").clicked() {
                            act = Some(Act::Push);
                        }
                        let pull_ok = idle && s.upstream && !dirty && !s.changed;
                        let tip = if dirty || s.changed { "Commit your changes before pulling" } else { "Bring in commits made elsewhere (only when this copy has none of its own)" };
                        if ui.add_enabled(pull_ok, egui::Button::new(if s.behind > 0 { format!("Pull ({})", s.behind) } else { "Pull".into() })).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                            act = Some(Act::Pull);
                        }
                    });
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
                    if b.on_disabled_hover_text("Sign in to GitHub below first").clicked() {
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
            match &v.account {
                Some((u, src)) => {
                    ui.horizontal_wrapped(|ui| {
                        let from = match src {
                            TokenSource::Saved => "",
                            TokenSource::Env => " (token from the environment)",
                            TokenSource::GhCli => " (from the GitHub CLI)",
                        };
                        ui.label(RichText::new(format!("Signed in as {}{from}", u.login)).color(theme::muted()).size(11.5));
                        if *src == TokenSource::Saved && ui.small_button("Sign out").clicked() {
                            act = Some(Act::SignOut);
                        }
                    });
                }
                None if v.account_rx.is_some() => {
                    ui.label(RichText::new("Checking the GitHub sign-in…").color(theme::muted()).size(11.5));
                }
                None => {
                    egui::CollapsingHeader::new(RichText::new("Sign in to GitHub").size(12.0)).id_salt("vcs-signin").default_open(s.remote.is_none()).show(ui, |ui| {
                        ui.label(
                            RichText::new("Paste a personal access token with the \"repo\" scope. Without one, Push and Pull use git's own sign-in (an SSH key or credential manager).")
                                .color(theme::muted())
                                .size(11.5),
                        );
                        ui.hyperlink_to("Create a token on GitHub", "https://github.com/settings/tokens/new?scopes=repo&description=rCAD");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut v.token_input).password(true).hint_text("ghp_… or github_pat_…").desired_width(230.0));
                            if ui.add_enabled(!v.token_input.trim().is_empty() && v.busy.is_none(), egui::Button::new("Sign in")).clicked() {
                                act = Some(Act::SignIn);
                            }
                        });
                    });
                }
            }
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
        Act::Push => {
            let Some(root) = app.vcs.status.as_ref().map(|s| s.root.clone()) else { return };
            run(app, "Pushing…", move || vcs::push(&root, token().as_deref()).map(Done::Note));
        }
        Act::Pull => {
            let Some(root) = app.vcs.status.as_ref().map(|s| s.root.clone()) else { return };
            run(app, "Pulling…", move || vcs::pull(&root, token().as_deref()).map(|m| Done::Pulled(m, path)));
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
                    Ok(()) => Some(("Connected: push to send your commits there".into(), false)),
                    Err(e) => Some((e, true)),
                };
            }
            refresh(app);
        }
        Act::SignIn => {
            let t = app.vcs.token_input.trim().to_string();
            let (tx, rx) = channel();
            app.vcs.note = None;
            app.vcs.busy = Some(("Signing in…".into(), rx));
            let (atx, arx) = channel();
            app.vcs.account_rx = Some(arx);
            std::thread::spawn(move || match vcs::gh_user(&t).and_then(|u| vcs::save_token(&t).map(|_| u)) {
                Ok(u) => {
                    let _ = tx.send(Ok(Done::Note(format!("Signed in to GitHub as {}", u.login))));
                    let _ = atx.send(Done::Account(Some((u, TokenSource::Saved))));
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    let _ = atx.send(Done::Account(None));
                }
            });
            app.vcs.token_input.clear();
        }
        Act::SignOut => {
            let _ = vcs::save_token("");
            app.vcs.account = None;
            // a token from the environment or the GitHub CLI may still apply
            check_account(app);
        }
    }
}
