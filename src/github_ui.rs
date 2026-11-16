//! The GitHub account window (Sign in with GitHub through the device flow, or a pasted token)
//! and the Open from GitHub window, which lists the account's repositories and clones one into
//! the projects folder, so a project published from one computer opens on another. The account
//! itself is kept by `vcs_ui` (`app.vcs.account`); the git and GitHub work is in `vcs.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use egui::{Align, Frame, Layout, Margin, RichText, Ui};

use crate::app::App;
use crate::icons;
use crate::theme;
use crate::vcs::{self, Cloned, DeviceCode, DevicePoll, GhRepo, GhUser, TokenSource};

/// A device-flow sign-in under way.
struct Device {
    code: DeviceCode,
    started: Instant,
    /// The signed-in account (or why it failed).
    rx: Receiver<Result<GhUser, String>>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct GithubUi {
    /// The account window.
    pub account_open: bool,
    /// Asking GitHub for a code.
    starting: Option<Receiver<Result<DeviceCode, String>>>,
    device: Option<Device>,
    token_input: String,
    checking_token: Option<Receiver<Result<GhUser, String>>>,
    error: Option<String>,
    /// The Open from GitHub window.
    pub repos_open: bool,
    repos: Option<Result<Vec<GhRepo>, String>>,
    repos_rx: Option<Receiver<Result<Vec<GhRepo>, String>>>,
    /// The account the list is for.
    repos_for: Option<String>,
    filter: String,
    url: String,
    /// A clone under way: which repository, and where its result arrives.
    cloning: Option<(String, Receiver<Result<Cloned, String>>)>,
    clone_error: Option<String>,
}

pub fn open_account(app: &mut App) {
    app.github.account_open = true;
    app.github.error = None;
    if !app.vcs.account_checked {
        crate::vcs_ui::check_account(app);
    }
}

pub fn open_repos(app: &mut App) {
    app.github.repos_open = true;
    app.github.clone_error = None;
    if !app.vcs.account_checked {
        crate::vcs_ui::check_account(app);
    }
}

/// Runs every frame (on the start screen too).
pub fn frame(app: &mut App, ctx: &egui::Context) {
    poll(app, ctx);
    if app.github.account_open {
        account_window(app, ctx);
    }
    if app.github.repos_open {
        repos_window(app, ctx);
    }
}

fn signed_in(app: &mut App, u: GhUser) {
    app.vcs.account = Some((u.clone(), TokenSource::Saved));
    app.vcs.account_checked = true;
    app.vcs.account_rx = None;
    app.github.repos = None;
    app.flash_info(format!("Signed in to GitHub as {}", u.login), Some(Duration::from_secs(3)));
}

fn poll(app: &mut App, ctx: &egui::Context) {
    let g = &mut app.github;
    let busy = g.starting.is_some() || g.device.is_some() || g.checking_token.is_some() || g.repos_rx.is_some() || g.cloning.is_some();
    if busy {
        ctx.request_repaint_after(Duration::from_millis(250));
    }
    if let Some(rx) = &g.starting {
        if let Ok(r) = rx.try_recv() {
            g.starting = None;
            match r {
                Ok(code) => {
                    // the code is in the clipboard and GitHub's page opens, ready to paste it
                    ctx.copy_text(code.user_code.clone());
                    crate::printing::open_url(&code.verification_uri);
                    g.device = Some(wait_for_approval(code));
                }
                Err(e) => g.error = Some(e),
            }
        }
    }
    if let Some(d) = &g.device {
        match d.rx.try_recv() {
            Ok(Ok(u)) => {
                g.device = None;
                g.account_open = false;
                signed_in(app, u);
                return;
            }
            Ok(Err(e)) => {
                g.device = None;
                g.error = Some(e);
            }
            Err(_) => {}
        }
    }
    let g = &mut app.github;
    if let Some(rx) = &g.checking_token {
        if let Ok(r) = rx.try_recv() {
            g.checking_token = None;
            match r {
                Ok(u) => {
                    g.account_open = false;
                    signed_in(app, u);
                    return;
                }
                Err(e) => g.error = Some(e),
            }
        }
    }
    let g = &mut app.github;
    if let Some(rx) = &g.repos_rx {
        if let Ok(r) = rx.try_recv() {
            g.repos_rx = None;
            g.repos = Some(r);
        }
    }
    if let Some((name, rx)) = &g.cloning {
        if let Ok(r) = rx.try_recv() {
            let name = name.clone();
            g.cloning = None;
            match r {
                Ok(Cloned::Document(p)) => {
                    g.repos_open = false;
                    app.guard(crate::session::Pending::OpenPath(p));
                    app.flash_info(format!("Opened {name} from GitHub · it syncs on open and after each save"), Some(Duration::from_secs(4)));
                }
                Ok(Cloned::Empty(p)) => {
                    // an empty repository: a new document saved (and so committed and pushed) there
                    g.repos_open = false;
                    app.guard(crate::session::Pending::NewProject(p, false));
                    app.flash_info(format!("{name} was empty: started a new document in it"), Some(Duration::from_secs(4)));
                }
                Err(e) => g.clone_error = Some(e),
            }
        }
    }
}

/// Poll GitHub in the background until the user approves the code (or it expires).
fn wait_for_approval(code: DeviceCode) -> Device {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    let (device_code, mut interval, expires) = (code.device_code.clone(), code.interval, code.expires_in);
    std::thread::spawn(move || {
        let Some(id) = vcs::client_id() else { return };
        let end = Instant::now() + Duration::from_secs(expires);
        let r = loop {
            // sleep in small steps, so Cancel stops it at once
            let wake = Instant::now() + Duration::from_secs(interval);
            while Instant::now() < wake {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if Instant::now() > end {
                break Err("the code expired: start again".to_string());
            }
            match vcs::device_poll(&id, &device_code) {
                Ok(DevicePoll::Waiting) => {}
                Ok(DevicePoll::SlowDown) => interval += 5,
                Ok(DevicePoll::Token(t)) => break vcs::gh_user(&t).and_then(|u| vcs::save_token(&t).map(|_| u)),
                // a dropped connection: keep trying until the code expires
                Err(e) if e.starts_with("could not reach") => {}
                Err(e) => break Err(e),
            }
        };
        if !stop.load(Ordering::Relaxed) {
            let _ = tx.send(r);
        }
    });
    Device { code, started: Instant::now(), rx, cancel }
}

fn start_device_flow(app: &mut App) {
    let Some(id) = vcs::client_id() else { return };
    let (tx, rx) = channel();
    app.github.error = None;
    app.github.starting = Some(rx);
    std::thread::spawn(move || {
        let _ = tx.send(vcs::device_start(&id));
    });
}

fn cancel_device(app: &mut App) {
    if let Some(d) = app.github.device.take() {
        d.cancel.store(true, Ordering::Relaxed);
    }
    app.github.starting = None;
}

pub fn sign_out(app: &mut App) {
    cancel_device(app);
    let _ = vcs::save_token("");
    app.vcs.account = None;
    app.github.repos = None;
    // a token from the environment or the GitHub CLI may still apply
    crate::vcs_ui::check_account(app);
}

fn muted(ui: &mut Ui, t: impl Into<String>) {
    ui.label(RichText::new(t).color(theme::muted()).size(11.5));
}

enum AccountAct {
    Start,
    Cancel,
    Token,
    SignOut,
    Repos,
}

fn account_window(app: &mut App, ctx: &egui::Context) {
    let mut open = true;
    let mut act = None;
    let has_client = vcs::client_id().is_some();
    egui::Window::new("GitHub account")
        .id(egui::Id::new("github-account"))
        .open(&mut open)
        .default_pos(ctx.content_rect().center() - egui::vec2(200.0, 160.0))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(400.0);
            let g = &mut app.github;
            if let Some(d) = &g.device {
                ui.label("Enter this code on GitHub to link your account:");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    Frame::new().fill(theme::panel_2()).corner_radius(6).inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                        ui.label(RichText::new(&d.code.user_code).monospace().size(24.0).strong());
                    });
                    ui.vertical(|ui| {
                        if ui.add(egui::Button::image_and_text(icons::image("copy", 14.0).tint(theme::text()), "Copy code")).clicked() {
                            ui.ctx().copy_text(d.code.user_code.clone());
                        }
                        if ui.button("Open GitHub").clicked() {
                            crate::printing::open_url(&d.code.verification_uri);
                        }
                    });
                });
                ui.add_space(4.0);
                muted(ui, format!("The code is copied, and {} opened in your browser. Paste it there and press Authorize.", d.code.verification_uri));
                ui.horizontal(|ui| {
                    ui.spinner();
                    let left = d.code.expires_in.saturating_sub(d.started.elapsed().as_secs());
                    muted(ui, format!("Waiting for GitHub… (the code works for {} more min)", left.div_ceil(60)));
                });
                if ui.button("Cancel").clicked() {
                    act = Some(AccountAct::Cancel);
                }
            } else if let Some((u, src)) = &app.vcs.account {
                ui.horizontal(|ui| {
                    ui.add(icons::image("check", 16.0).tint(theme::ok_green()));
                    ui.label(RichText::new(format!("Signed in as {}", u.login)).strong());
                    if let Some(n) = &u.name {
                        ui.label(RichText::new(n).color(theme::muted()));
                    }
                });
                match src {
                    TokenSource::Saved => muted(ui, "rCAD pushes and pulls your projects with this account, on this computer."),
                    TokenSource::Env => muted(ui, "The token comes from GITHUB_TOKEN or GH_TOKEN. Sign in below to use another account."),
                    TokenSource::GhCli => muted(ui, "The token comes from the GitHub CLI (gh). Sign in below to use another account."),
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Open a project from GitHub…").clicked() {
                        act = Some(AccountAct::Repos);
                    }
                    if *src == TokenSource::Saved && ui.button("Sign out").clicked() {
                        act = Some(AccountAct::SignOut);
                    }
                });
                if *src != TokenSource::Saved {
                    ui.add_space(6.0);
                    sign_in_ui(ui, g, has_client, &mut act);
                }
            } else if app.vcs.account_rx.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    muted(ui, "Checking the GitHub sign-in…");
                });
            } else {
                ui.label(
                    "Link your GitHub account to keep projects on GitHub and work on them from any computer: publish a project, then open it from the start screen elsewhere.",
                );
                ui.add_space(6.0);
                sign_in_ui(ui, g, has_client, &mut act);
            }
            if let Some(e) = &g.error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).color(theme::error()).size(11.5));
            }
        });
    if !open {
        app.github.account_open = false;
        cancel_device(app);
    }
    match act {
        Some(AccountAct::Start) => start_device_flow(app),
        Some(AccountAct::Cancel) => cancel_device(app),
        Some(AccountAct::SignOut) => sign_out(app),
        Some(AccountAct::Repos) => {
            app.github.account_open = false;
            open_repos(app);
        }
        Some(AccountAct::Token) => {
            let t = app.github.token_input.trim().to_string();
            app.github.token_input.clear();
            app.github.error = None;
            let (tx, rx) = channel();
            app.github.checking_token = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(vcs::gh_user(&t).and_then(|u| vcs::save_token(&t).map(|_| u)));
            });
        }
        None => {}
    }
}

/// The Sign in with GitHub button, with pasting a token as the other way in.
fn sign_in_ui(ui: &mut Ui, g: &mut GithubUi, has_client: bool, act: &mut Option<AccountAct>) {
    if has_client {
        let starting = g.starting.is_some();
        ui.horizontal(|ui| {
            let b = egui::Button::image_and_text(
                icons::image("branch", 16.0).tint(egui::Color32::WHITE),
                RichText::new("Sign in with GitHub").family(theme::medium()).color(egui::Color32::WHITE),
            )
            .fill(theme::accent());
            if ui.add_enabled(!starting, b).clicked() {
                *act = Some(AccountAct::Start);
            }
            if starting {
                ui.spinner();
            }
        });
        muted(ui, "GitHub shows a code to enter in your browser; nothing to copy by hand.");
        ui.add_space(4.0);
    }
    let token_ui = |ui: &mut Ui, g: &mut GithubUi, act: &mut Option<AccountAct>| {
        muted(ui, "A personal access token with the \"repo\" scope (classic), or a fine-grained one with Contents read and write.");
        ui.hyperlink_to(RichText::new("Create a token on GitHub").size(11.5), "https://github.com/settings/tokens/new?scopes=repo&description=rCAD");
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut g.token_input).password(true).hint_text("ghp_… or github_pat_…").desired_width(250.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can = !g.token_input.trim().is_empty() && g.checking_token.is_none();
            if (ui.add_enabled(can, egui::Button::new("Sign in")).clicked() || enter) && can {
                *act = Some(AccountAct::Token);
            }
            if g.checking_token.is_some() {
                ui.spinner();
            }
        });
    };
    if has_client {
        egui::CollapsingHeader::new(RichText::new("Use a personal access token instead").size(12.0)).id_salt("gh-token").show(ui, |ui| token_ui(ui, g, act));
    } else {
        token_ui(ui, g, act);
    }
}

/// The account chip in the start screen's header.
pub fn chip(app: &mut App, ui: &mut Ui) {
    if !app.vcs.account_checked {
        crate::vcs_ui::check_account(app);
    }
    let (text, tip) = match &app.vcs.account {
        Some((u, _)) => (u.login.clone(), "GitHub account".to_string()),
        None => ("Sign in with GitHub".to_string(), "Link your GitHub account to sync projects between computers".to_string()),
    };
    let signed = app.vcs.account.is_some();
    let r = ui
        .add(egui::Button::image_and_text(
            icons::image(if signed { "check" } else { "branch" }, 14.0).tint(if signed { theme::ok_green() } else { theme::text() }),
            RichText::new(text).size(12.5),
        ))
        .on_hover_text(tip);
    if r.clicked() {
        open_account(app);
    }
}

fn load_repos(app: &mut App) {
    let Some((u, _)) = &app.vcs.account else { return };
    app.github.repos_for = Some(u.login.clone());
    let (tx, rx) = channel();
    app.github.repos_rx = Some(rx);
    std::thread::spawn(move || {
        let r = vcs::token().ok_or_else(|| "sign in to GitHub first".to_string()).and_then(|(t, _)| vcs::gh_repos(&t));
        let _ = tx.send(r);
    });
}

fn start_clone(app: &mut App, name: String, url: String) {
    let (tx, rx) = channel();
    app.github.clone_error = None;
    app.github.cloning = Some((name, rx));
    let parent = crate::project::default_dir();
    std::thread::spawn(move || {
        let token = vcs::token().map(|(t, _)| t);
        let _ = tx.send(vcs::clone_project(&url, token.as_deref(), &parent));
    });
}

/// "2026-11-10T21:59:29Z" as "10 Nov 2026".
fn date(iso: &str) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let p: Vec<&str> = iso.get(..10).unwrap_or("").split('-').collect();
    match (p.first(), p.get(1).and_then(|m| m.parse::<usize>().ok()), p.get(2).and_then(|d| d.parse::<u32>().ok())) {
        (Some(y), Some(m @ 1..=12), Some(d)) => format!("{d} {} {y}", M[m - 1]),
        _ => String::new(),
    }
}

fn repos_window(app: &mut App, ctx: &egui::Context) {
    if app.vcs.git.is_none() {
        app.vcs.git = Some(vcs::version());
    }
    let login = app.vcs.account.as_ref().map(|(u, _)| u.login.clone());
    if login.is_some() && app.github.repos_rx.is_none() && (app.github.repos.is_none() || app.github.repos_for != login) {
        load_repos(app);
    }
    let mut open = true;
    let mut clone: Option<(String, String)> = None;
    let mut sign_in = false;
    let mut reload = false;
    let no_git = app.vcs.git.as_ref().is_some_and(|g| g.is_none());
    egui::Window::new("Open from GitHub")
        .id(egui::Id::new("github-repos"))
        .open(&mut open)
        .default_size([480.0, 520.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(240.0, 260.0))
        .collapsible(false)
        .resizable(true)
        .show(ctx, |ui| {
            let g = &mut app.github;
            if no_git {
                ui.label("Opening projects from GitHub uses git, which isn't installed.");
                ui.hyperlink_to("Download git", "https://git-scm.com/downloads");
                return;
            }
            muted(ui, format!("The project is copied into {} and kept in sync: it pulls when opened and pushes after each save.", crate::project::default_dir().display()));
            ui.add_space(4.0);
            let busy = g.cloning.is_some();
            match &login {
                None => {
                    ui.horizontal(|ui| {
                        ui.label("Sign in to list your repositories.");
                        if ui.button("Sign in with GitHub…").clicked() {
                            sign_in = true;
                        }
                    });
                }
                Some(l) => {
                    ui.horizontal(|ui| {
                        theme::field_style(ui);
                        ui.add(icons::image("search", 14.0).tint(theme::muted()));
                        ui.add(egui::TextEdit::singleline(&mut g.filter).hint_text(format!("Search {l}'s repositories")).desired_width(260.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add_enabled(g.repos_rx.is_none(), egui::Button::new("Refresh")).clicked() {
                                reload = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    let list_h = (ui.available_height() - 90.0).max(160.0);
                    egui::ScrollArea::vertical().id_salt("gh-repos").auto_shrink([false, false]).max_height(list_h).show(ui, |ui| match &g.repos {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                muted(ui, "Loading repositories…");
                            });
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e).color(theme::error()));
                        }
                        Some(Ok(list)) => {
                            let f = g.filter.to_lowercase();
                            let shown: Vec<&GhRepo> =
                                list.iter().filter(|r| f.is_empty() || r.full_name.to_lowercase().contains(&f) || r.description.to_lowercase().contains(&f)).collect();
                            if shown.is_empty() {
                                muted(ui, if list.is_empty() { "No repositories yet: publish a project from Version control first." } else { "No repository matches the search." });
                            }
                            for r in shown {
                                Frame::new().inner_margin(Margin::symmetric(4, 4)).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(RichText::new(&r.full_name).strong());
                                                if r.rcad {
                                                    ui.label(RichText::new("rCAD").size(10.5).color(theme::accent()));
                                                }
                                                if r.private {
                                                    ui.label(RichText::new("private").size(10.5).color(theme::muted()));
                                                }
                                            });
                                            let mut details = date(&r.pushed);
                                            if !r.description.is_empty() {
                                                details = if details.is_empty() { r.description.clone() } else { format!("{} · {details}", r.description) };
                                            }
                                            if !details.is_empty() {
                                                muted(ui, details);
                                            }
                                        });
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            let this = g.cloning.as_ref().is_some_and(|(n, _)| *n == r.full_name);
                                            if this {
                                                ui.spinner();
                                            } else if ui.add_enabled(!busy, egui::Button::new("Open")).clicked() {
                                                clone = Some((r.full_name.clone(), r.clone_url.clone()));
                                            }
                                        });
                                    });
                                });
                                ui.separator();
                            }
                        }
                    });
                }
            }
            ui.add_space(4.0);
            egui::CollapsingHeader::new(RichText::new("Open from a URL").size(12.0)).id_salt("gh-url").default_open(login.is_none()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut g.url).hint_text("https://github.com/you/project.git").desired_width(300.0));
                    if ui.add_enabled(!busy && !g.url.trim().is_empty(), egui::Button::new("Open")).clicked() {
                        clone = Some((vcs::short_remote(g.url.trim()), g.url.trim().to_string()));
                    }
                });
            });
            if let Some((n, _)) = &g.cloning {
                ui.horizontal(|ui| {
                    ui.spinner();
                    muted(ui, format!("Copying {n}…"));
                });
            }
            if let Some(e) = &g.clone_error {
                ui.label(RichText::new(e).color(theme::error()).size(11.5));
            }
        });
    if !open {
        app.github.repos_open = false;
    }
    if sign_in {
        open_account(app);
    }
    if reload {
        app.github.repos = None;
        load_repos(app);
    }
    if let Some((name, url)) = clone {
        start_clone(app, name, url);
    }
}
