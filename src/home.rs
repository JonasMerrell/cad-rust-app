//! Start screen: recently opened and saved documents with thumbnails, plus New and Open.
//! Shown on launch without a file and from the header's home button. The list lives in
//! `~/.config/cad-rust-app/recent.json`, thumbnails in `~/.config/cad-rust-app/thumbs/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Align, Color32, CornerRadius, Frame, Key, Layout, Margin, RichText, Sense, Stroke, Ui};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::icons;
use crate::theme;

const MAX_RECENT: usize = 30;
const THUMB_W: u32 = 480;
const THUMB_H: u32 = 300;

#[derive(Clone, Serialize, Deserialize)]
pub struct Recent {
    pub path: PathBuf,
    /// When the document was last opened or saved here (unix seconds).
    pub used: u64,
}

pub struct HomeUi {
    pub open: bool,
    pub recents: Vec<Recent>,
    pub filter: String,
    /// A document whose thumbnail should be taken once the model is on screen, and since when.
    thumb_for: Option<(PathBuf, Instant)>,
    /// Screenshot requested for that thumbnail, waiting for the image.
    thumb_waiting: bool,
    /// Since when the model has been rebuilt and framed, so the camera has settled.
    ready_since: Option<Instant>,
    /// Loaded thumbnails by document, with the thumbnail file's mtime.
    textures: HashMap<PathBuf, Option<(SystemTime, egui::TextureHandle)>>,
}

impl Default for HomeUi {
    fn default() -> Self {
        HomeUi { open: false, recents: load(), filter: String::new(), thumb_for: None, thumb_waiting: false, ready_since: None, textures: HashMap::new() }
    }
}

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cad-rust-app"))
}

fn list_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("recent.json"))
}

fn thumb_path(doc: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    doc.hash(&mut h);
    config_dir().map(|d| d.join("thumbs").join(format!("{:016x}.png", h.finish())))
}

fn load() -> Vec<Recent> {
    list_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(list: &[Recent]) {
    if let Some(p) = list_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if let Ok(s) = serde_json::to_string_pretty(list) {
            let _ = std::fs::write(p, s);
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A document was opened or saved: move it to the top of the list and take a new thumbnail.
pub fn touch(app: &mut App, p: &Path) {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let h = &mut app.home;
    h.recents.retain(|r| r.path != p);
    h.recents.insert(0, Recent { path: p.clone(), used: now() });
    h.recents.truncate(MAX_RECENT);
    save(&h.recents);
    h.thumb_for = Some((p, Instant::now()));
    h.thumb_waiting = false;
}

fn forget(app: &mut App, p: &Path) {
    app.home.recents.retain(|r| r.path != p);
    save(&app.home.recents);
    if let Some(t) = thumb_path(p) {
        let _ = std::fs::remove_file(t);
    }
    app.home.textures.remove(p);
}

/// The viewport shows no hover highlight while a thumbnail is about to be taken.
pub fn hide_hover(app: &App) -> bool {
    app.home.thumb_for.is_some() && app.home.ready_since.is_some()
}

pub fn show_home(app: &mut App) {
    app.home.open = true;
    app.home.filter.clear();
}

/// Take the pending thumbnail once the model has been rebuilt and framed. Runs every frame.
pub fn thumbnail(app: &mut App, ctx: &egui::Context) {
    let Some((doc, since)) = app.home.thumb_for.clone() else { return };
    if app.home.thumb_waiting {
        let img = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(img) = img else { return };
        app.home.thumb_for = None;
        app.home.thumb_waiting = false;
        let region = img.region(&app.viewport_rect, Some(ctx.pixels_per_point()));
        let (w, h) = (region.size[0] as u32, region.size[1] as u32);
        let bytes: Vec<u8> = region.pixels.iter().flat_map(|c| c.to_array()).collect();
        let (Some(buf), Some(out)) = (image::RgbaImage::from_raw(w, h, bytes), thumb_path(&doc)) else { return };
        // the middle of the viewport, leaving out the view cube, the side toolbar and the hint line
        let (cw, h) = (w * 4 / 5, h * 92 / 100);
        if cw < 16 || h < 16 {
            return;
        }
        let crop = image::imageops::crop_imm(&buf, (w - cw) / 2, 0, cw, h).to_image();
        // fit it into the card, widening each row with its own edge colour (the background is a vertical gradient)
        let scale = (THUMB_W as f32 / cw as f32).min(THUMB_H as f32 / h as f32);
        let (sw, sh) = (((cw as f32 * scale) as u32).clamp(1, THUMB_W), ((h as f32 * scale) as u32).clamp(1, THUMB_H));
        let fitted = image::imageops::resize(&crop, sw, sh, image::imageops::FilterType::Triangle);
        let (ox, oy) = ((THUMB_W - sw) / 2, (THUMB_H - sh) / 2);
        let small = image::RgbaImage::from_fn(THUMB_W, THUMB_H, |x, y| {
            let fy = y.saturating_sub(oy).min(sh - 1);
            let fx = if x < ox { 0 } else { (x - ox).min(sw - 1) };
            *fitted.get_pixel(fx, fy)
        });
        let _ = std::fs::create_dir_all(out.parent().unwrap());
        if small.save(&out).is_ok() {
            app.home.textures.remove(&doc);
        }
        return;
    }
    if since.elapsed() > Duration::from_secs(60) {
        app.home.thumb_for = None;
        return;
    }
    // wait for the rebuild and for the camera to settle on the model
    let ready = app.kernel.state == crate::worker::KernelState::Ready && !app.dirty_model && !app.first_fit;
    let ready = ready && !app.home.open && app.screenshot_pending.is_none() && app.viewport_rect.width() > 50.0;
    if !ready {
        app.home.ready_since = None;
    } else if app.home.ready_since.is_none() {
        app.home.ready_since = Some(Instant::now());
    }
    if !app.home.ready_since.is_some_and(|t| t.elapsed() > Duration::from_millis(700)) {
        ctx.request_repaint_after(Duration::from_millis(250));
        return;
    }
    app.home.thumb_waiting = true;
    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
}

fn texture(h: &mut HomeUi, ctx: &egui::Context, doc: &Path) -> Option<egui::TextureHandle> {
    let file = thumb_path(doc)?;
    let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
    if let Some(cached) = h.textures.get(doc) {
        match (cached, mtime) {
            (Some((t, tex)), Some(m)) if *t == m => return Some(tex.clone()),
            (None, None) => return None,
            _ => {}
        }
    }
    let loaded = mtime.and_then(|m| {
        let img = image::open(&file).ok()?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        let tex = ctx.load_texture(format!("thumb:{}", doc.display()), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
        Some((m, tex))
    });
    let tex = loaded.as_ref().map(|(_, t)| t.clone());
    h.textures.insert(doc.to_path_buf(), loaded);
    tex
}

/// "Just now", "5 min ago", "Yesterday", "12 Oct", "3 Mar 2025".
fn when(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let ago = now().saturating_sub(secs);
    match ago {
        0..60 => "Just now".into(),
        60..3600 => format!("{} min ago", ago / 60),
        3600..86400 => format!("{} h ago", ago / 3600),
        86400..172800 => "Yesterday".into(),
        172800..604800 => format!("{} days ago", ago / 86400),
        _ => {
            let (y, m, d) = civil(secs as i64 / 86400);
            let (ny, _, _) = civil(now() as i64 / 86400);
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            if y == ny { format!("{d} {}", MONTHS[m as usize - 1]) } else { format!("{d} {} {y}", MONTHS[m as usize - 1]) }
        }
    }
}

/// Days since 1970-01-01 to (year, month, day).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn display_name(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// The folder, with the home directory shortened to `~`.
fn folder(p: &Path) -> String {
    let dir = p.parent().map(|d| d.display().to_string()).unwrap_or_default();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && dir.starts_with(&h) => format!("~{}", &dir[h.len()..]),
        _ => dir,
    }
}

enum Action {
    New,
    Open,
    OpenPath(PathBuf),
    Forget(PathBuf),
    Back,
}

/// The whole window while the start screen is shown.
pub fn ui(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let has_doc = app.path.is_some() || app.ws.studios.iter().any(|s| !s.doc.features.is_empty());
    let mut action = None;
    let (ctrl, _) = crate::app::key_mods(&ctx);
    ctx.input(|i| {
        if ctrl && i.key_pressed(Key::O) {
            action = Some(Action::Open);
        } else if ctrl && i.key_pressed(Key::N) {
            action = Some(Action::New);
        } else if i.key_pressed(Key::Escape) && has_doc {
            action = Some(Action::Back);
        }
    });

    let header = Frame::new().fill(theme::header()).inner_margin(Margin { left: 12, right: 10, top: 5, bottom: 5 });
    egui::Panel::top("home-header").frame(header).resizable(false).show_separator_line(false).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add(icons::image("logo", 20.0).tint(theme::accent()));
            ui.label(RichText::new("rCAD").family(theme::medium()).size(15.0).color(theme::text()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (icon, tip) = if app.dark { ("sun", "Switch to light mode") } else { ("moon", "Switch to dark mode") };
                if ui.add(egui::Button::image(icons::image(icon, 17.0).tint(theme::text()))).on_hover_text(tip).clicked() {
                    app.set_dark(ui.ctx(), !app.dark);
                }
            });
        });
    });

    let narrow = ctx.content_rect().width() < 700.0;
    let body = egui::CentralPanel::default().frame(Frame::new().fill(theme::bg())).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let pad = if narrow { 16.0 } else { 40.0 };
            let width = (ui.available_width() - 2.0 * pad).min(1240.0);
            let left = ((ui.available_width() - width) / 2.0).max(pad);
            ui.add_space(if narrow { 16.0 } else { 32.0 });
            ui.horizontal(|ui| {
                ui.add_space(left);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    // title row with the actions
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Documents").family(theme::medium()).size(22.0).color(theme::text()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if has_doc {
                                if action_button(ui, "partStudio", "Back to document", false).on_hover_text("Esc").clicked() {
                                    action = Some(Action::Back);
                                }
                            }
                            if action_button(ui, "folder", "Open…", false).on_hover_text("Ctrl+O").clicked() {
                                action = Some(Action::Open);
                            }
                            if action_button(ui, "plus", "New document", true).on_hover_text("Ctrl+N").clicked() {
                                action = Some(Action::New);
                            }
                        });
                    });
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("RECENT").family(theme::medium()).size(10.5).color(theme::muted()));
                        if !app.home.recents.is_empty() {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                theme::field_style(ui);
                                ui.add(egui::TextEdit::singleline(&mut app.home.filter).hint_text("Search recent documents").desired_width(if narrow { 160.0 } else { 240.0 }));
                                ui.add(icons::image("search", 15.0).tint(theme::muted()));
                            });
                        }
                    });
                    ui.add_space(10.0);
                    let f = app.home.filter.to_lowercase();
                    let list: Vec<Recent> = app.home.recents.iter().filter(|r| f.is_empty() || r.path.to_string_lossy().to_lowercase().contains(&f)).cloned().collect();
                    if app.home.recents.is_empty() {
                        empty(ui, "No recent documents yet", "Documents you open or save show up here.");
                    } else if list.is_empty() {
                        empty(ui, "No matches", "No recent document matches the search.");
                    } else {
                        grid(app, ui, &list, width, &mut action);
                    }
                    ui.add_space(24.0);
                });
            });
        });
    });
    // toasts (errors from opening) are placed over the viewport rect
    app.viewport_rect = body.response.rect;

    match action {
        // ask about unsaved changes first; the start screen closes once the document is replaced
        Some(Action::New) => app.guard(crate::session::Pending::New),
        Some(Action::Open) => app.guard(crate::session::Pending::Open),
        Some(Action::OpenPath(p)) => app.guard(crate::session::Pending::OpenPath(p)),
        Some(Action::Forget(p)) => forget(app, &p),
        Some(Action::Back) => app.home.open = false,
        None => {}
    }
}

fn action_button(ui: &mut Ui, icon: &str, label: &str, primary: bool) -> egui::Response {
    let (fill, fg) = if primary { (theme::accent(), Color32::WHITE) } else { (theme::panel(), theme::text()) };
    let b = egui::Button::image_and_text(icons::image(icon, 16.0).tint(fg), RichText::new(label).family(theme::medium()).color(fg))
        .fill(fill)
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.0, theme::border()) })
        .corner_radius(6)
        .min_size(egui::vec2(0.0, 32.0));
    ui.add(b)
}

fn empty(ui: &mut Ui, title: &str, sub: &str) {
    Frame::new().fill(theme::panel()).stroke(Stroke::new(1.0, theme::border())).corner_radius(10).inner_margin(Margin::same(32)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.add(icons::image("file", 36.0).tint(theme::muted()));
            ui.add_space(8.0);
            ui.label(RichText::new(title).family(theme::medium()).size(15.0).color(theme::text()));
            ui.label(RichText::new(sub).color(theme::muted()));
        });
    });
}

fn grid(app: &mut App, ui: &mut Ui, list: &[Recent], width: f32, action: &mut Option<Action>) {
    let gap = 16.0;
    let cols = ((width + gap) / (220.0 + gap)).floor().max(1.0) as usize;
    let card_w = (width - gap * (cols - 1) as f32) / cols as f32;
    let thumb_h = card_w * THUMB_H as f32 / THUMB_W as f32;
    let card_h = thumb_h + 58.0;
    let ctx = ui.ctx().clone();
    for row in list.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for r in row {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(card_w, card_h), Sense::click());
                let exists = r.path.exists();
                let hovered = resp.hovered();
                let p = ui.painter();
                let radius = CornerRadius::same(10);
                p.rect(rect, radius, theme::panel(), Stroke::new(1.0, if hovered && exists { theme::accent() } else { theme::border() }), egui::StrokeKind::Inside);
                // thumbnail
                let trect = egui::Rect::from_min_size(rect.min, egui::vec2(card_w, thumb_h)).shrink(1.0);
                let top = CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 };
                p.rect_filled(trect, top, theme::vp::background());
                match texture(&mut app.home, &ctx, &r.path) {
                    Some(tex) => {
                        let tint = if exists { Color32::WHITE } else { Color32::from_white_alpha(90) };
                        egui::Image::new(&tex).corner_radius(top).tint(tint).paint_at(ui, trect);
                    }
                    None => {
                        let c = trect.center();
                        icons::image("cube", 40.0).tint(theme::muted().gamma_multiply(0.6)).paint_at(ui, egui::Rect::from_center_size(c, egui::vec2(40.0, 40.0)));
                    }
                }
                let p = ui.painter();
                p.hline(rect.x_range(), trect.bottom(), Stroke::new(1.0, theme::border()));
                // name, folder, date
                let x = rect.left() + 12.0;
                let w = card_w - 24.0;
                let name = elide(ui, &display_name(&r.path), w, egui::FontId::new(13.5, theme::medium()));
                p.text(
                    egui::pos2(x, trect.bottom() + 10.0),
                    egui::Align2::LEFT_TOP,
                    name,
                    egui::FontId::new(13.5, theme::medium()),
                    if exists { theme::text() } else { theme::muted() },
                );
                let modified = std::fs::metadata(&r.path).and_then(|m| m.modified()).ok();
                let sub = match modified {
                    Some(m) => format!("{} · {}", when(m), folder(&r.path)),
                    None => "File not found".into(),
                };
                let sub = elide(ui, &sub, w, egui::FontId::proportional(11.5));
                p.text(egui::pos2(x, trect.bottom() + 30.0), egui::Align2::LEFT_TOP, sub, egui::FontId::proportional(11.5), if exists { theme::muted() } else { theme::error() });
                let resp = resp.on_hover_text(r.path.display().to_string());
                // remove from the list
                let x_rect = egui::Rect::from_min_size(egui::pos2(trect.right() - 30.0, trect.top() + 6.0), egui::vec2(24.0, 24.0));
                let x_resp = ui.interact(x_rect, ui.id().with(("forget", &r.path)), Sense::click());
                if hovered || x_resp.hovered() || !exists {
                    let p = ui.painter();
                    p.circle_filled(x_rect.center(), 11.0, if x_resp.hovered() { theme::panel_2() } else { theme::panel() });
                    icons::image("close", 13.0).tint(theme::text()).paint_at(ui, x_rect.shrink(5.5));
                }
                if x_resp.on_hover_text("Remove from recent").clicked() {
                    *action = Some(Action::Forget(r.path.clone()));
                } else if resp.clicked() {
                    *action = Some(Action::OpenPath(r.path.clone()));
                }
                if hovered && exists {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        });
        ui.add_space(gap);
    }
}

/// Cut `s` to fit `w` points, ending in an ellipsis.
fn elide(ui: &Ui, s: &str, w: f32, font: egui::FontId) -> String {
    let fits = |t: &str| ui.fonts_mut(|f| f.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x) <= w;
    if fits(s) {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut n = chars.len();
    while n > 1 {
        n -= 1;
        let t: String = chars[..n].iter().collect::<String>() + "…";
        if fits(&t) {
            return t;
        }
    }
    "…".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_754), (2026, 10, 28));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}
