//! The Assembly tab's Animation section: motion sliders linked to joints, and timelines that play them.

use std::collections::HashMap;
use std::time::Instant;

use egui::{Align2, Color32, FontId, RichText, Sense, Stroke, Ui};
use glam::DVec3;

use crate::app::App;
use crate::asm_anim::{self, Animation, Clip, Ease, Link, Motion, Repeat, Target};
use crate::asm_ui::{asm_index, centroid_local, checkpoint};
use crate::assembly::{self, Assembly, Instance, MateKind};
use crate::theme;

pub struct AnimUi {
    /// The assembly the play head belongs to.
    asm: Option<usize>,
    /// The animation shown (id).
    pub anim: Option<u64>,
    /// Play head (s since the start, repeats not folded).
    pub t: f64,
    pub playing: bool,
    pub speed: f64,
    last: Option<Instant>,
    /// Motions whose links are open for editing.
    open: Vec<u64>,
    /// A clip being dragged along the timeline: (index, grab offset s).
    drag_clip: Option<(usize, f64)>,
    /// When each motion slider was last moved (a counter): the latest takes a joint two motions move.
    touched: HashMap<u64, f64>,
    clock: f64,
}

impl Default for AnimUi {
    fn default() -> Self {
        AnimUi { asm: None, anim: None, t: 0.0, playing: false, speed: 1.0, last: None, open: vec![], drag_clip: None, touched: HashMap::new(), clock: 0.0 }
    }
}

fn centers(app: &App, ai: usize) -> Vec<(u64, DVec3)> {
    app.ws.assemblies[ai].instances.iter().map(|x| (x.id, centroid_local(app, x))).collect()
}

/// Put the joints where the motions have them and solve the rest of the mates around them.
fn pose(app: &mut App, ai: usize, vals: &HashMap<u64, f64>, rank: &HashMap<u64, f64>) {
    let a = &app.ws.assemblies[ai];
    let drives = asm_anim::drives(a, &asm_anim::mate_values(a, vals, rank));
    for m in app.ws.assemblies[ai].motions.iter_mut() {
        if let Some(v) = vals.get(&m.id) {
            m.value = *v;
        }
    }
    if drives.is_empty() {
        return;
    }
    let c = centers(app, ai);
    let r = assembly::solve_driven(&mut app.ws.assemblies[ai], &[], &|x: &Instance| c.iter().find(|c| c.0 == x.id).map(|c| c.1).unwrap_or(DVec3::ZERO), &drives);
    app.asm.failed = r.failed;
    app.asm.clash_stale = true;
    app.asm.scene_dirty = true;
}

/// Pose the assembly at its motion sliders.
pub fn pose_sliders(app: &mut App, ai: usize) {
    let mut rank = app.asm.anim.touched.clone();
    let v = asm_anim::motion_values(&app.ws.assemblies[ai], &HashMap::new(), &mut rank);
    pose(app, ai, &v, &rank);
}

fn touch(st: &mut AnimUi, id: u64) {
    st.clock += 1.0;
    st.touched.insert(id, st.clock);
}

/// Pose the assembly at time `t` of animation `id`.
pub fn pose_at(app: &mut App, ai: usize, id: u64, t: f64) {
    let Some(anim) = app.ws.assemblies[ai].animations.iter().find(|x| x.id == id).cloned() else { return };
    let (v, r) = asm_anim::animation_values(&app.ws.assemblies[ai], &anim, asm_anim::fold_time(&anim, t));
    pose(app, ai, &v, &r);
}

/// Advance a playing animation (call every frame the Assembly tab shows).
pub fn tick(app: &mut App, ctx: &egui::Context) {
    let Some(ai) = asm_index(app) else { return };
    let st = &mut app.asm.anim;
    if !st.playing || st.asm != Some(ai) {
        st.last = None;
        return;
    }
    let Some(anim) = st.anim.and_then(|id| app.ws.assemblies.get(ai)?.animations.iter().find(|x| x.id == id)).cloned() else {
        st.playing = false;
        return;
    };
    let now = Instant::now();
    if let Some(l) = st.last {
        st.t += now.duration_since(l).as_secs_f64() * st.speed;
    }
    st.last = Some(now);
    let d = anim.duration();
    if anim.repeat == Repeat::Once && st.t >= d {
        st.t = d;
        st.playing = false;
        st.last = None;
    }
    let t = st.t;
    pose_at(app, ai, anim.id, t);
    ctx.request_repaint();
}

/// Start a new motion moving mate `mate` (from the mates list).
pub fn new_motion_for(app: &mut App, ai: usize, mate: u64) {
    checkpoint(app);
    let a = &mut app.ws.assemblies[ai];
    let Some(name) = a.mates.iter().find(|m| m.id == mate).map(|m| m.name.clone()) else { return };
    let link = asm_anim::new_link(a, Target::Mate(mate));
    let id = a.new_id();
    // start the slider where the joint is now
    let now = asm_anim::current_value(a, Target::Mate(mate)).unwrap_or(link.from);
    let span = link.to - link.from;
    let value = if span.abs() > 1e-9 { ((now - link.from) / span).clamp(0.0, 1.0) } else { 0.0 };
    a.motions.push(Motion { id, name, links: vec![link], value });
    app.asm.anim.open.push(id);
    touch(&mut app.asm.anim, id);
}

fn target_label(a: &Assembly, t: Target) -> String {
    let mate = |id: u64| a.mates.iter().find(|m| m.id == id);
    match t {
        Target::Mate(id) => match mate(id) {
            Some(m) if m.kind == MateKind::Cylindrical => format!("{} (travel)", m.name),
            Some(m) => m.name.clone(),
            None => "(deleted mate)".into(),
        },
        Target::Turn(id) => mate(id).map(|m| format!("{} (turn)", m.name)).unwrap_or_else(|| "(deleted mate)".into()),
        Target::Motion(id) => a.motions.iter().find(|m| m.id == id).map(|m| format!("Motion: {}", m.name)).unwrap_or_else(|| "(deleted motion)".into()),
    }
}

/// Everything a link can move, but motion `own` itself.
fn targets(a: &Assembly, own: u64) -> Vec<Target> {
    let mut out = vec![];
    for (id, _, kind) in asm_anim::movable_mates(a) {
        out.push(Target::Mate(id));
        if kind == MateKind::Cylindrical {
            out.push(Target::Turn(id));
        }
    }
    out.extend(a.motions.iter().filter(|m| m.id != own).map(|m| Target::Motion(m.id)));
    out
}

/// What the frame's edits want: an undo step first, a write-back, and which pose to show.
#[derive(Default)]
struct Edit {
    checkpoint: bool,
    changed: bool,
    sliders: bool,
    timeline: bool,
}

impl Edit {
    /// Note a widget's response: an undo step when an edit starts (a drag) or lands (typed in).
    fn track(&mut self, r: &egui::Response) -> bool {
        if r.drag_started() || (r.changed() && !r.dragged()) {
            self.checkpoint = true;
        }
        if r.changed() {
            self.changed = true;
        }
        r.changed()
    }
    fn click(&mut self) {
        self.checkpoint = true;
        self.changed = true;
    }
}

fn pct(ui: &mut Ui, v: &mut f64) -> egui::Response {
    let mut p = *v * 100.0;
    let r = ui.add(egui::DragValue::new(&mut p).speed(0.5).range(0.0..=100.0).max_decimals(1).suffix("%"));
    if r.changed() {
        *v = p / 100.0;
    }
    r
}

/// A turned triangle that opens and closes a row.
fn disclosure(ui: &mut Ui, open: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), Sense::click());
    let c = rect.center();
    let col = if r.hovered() { theme::text() } else { theme::muted() };
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)]
    } else {
        vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(3.0, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    r
}

fn small(text: &str) -> RichText {
    RichText::new(text).size(12.0).color(theme::muted())
}

fn x_button(ui: &mut Ui, tip: &str) -> bool {
    ui.add(egui::Button::image(crate::icons::image("close", 12.0).tint(theme::muted())).frame(false)).on_hover_text(tip).clicked()
}

pub fn section_ui(app: &mut App, ui: &mut Ui, ai: usize) {
    if app.asm.anim.asm != Some(ai) {
        app.asm.anim = AnimUi { asm: Some(ai), open: std::mem::take(&mut app.asm.anim.open), clock: app.asm.anim.clock, ..Default::default() };
    }
    let a = app.ws.assemblies[ai].clone();
    let mut motions = a.motions.clone();
    let mut anims = a.animations.clone();
    let mut e = Edit::default();
    let mut new_ids = 0u64;
    let mut next_id = || {
        new_ids += 1;
        a.next_id + new_ids
    };

    // ------------------------------------------------ motions
    ui.label(small("Motions: sliders that move joints. One slider can move several joints, at their own rates or one after another, and drive other motions."));
    ui.add_space(4.0);
    let driven = asm_anim::driven_motions(&a);
    let mut remove = None;
    for (k, m) in motions.iter_mut().enumerate() {
        let open = app.asm.anim.open.contains(&m.id);
        ui.horizontal(|ui| {
            if disclosure(ui, open).on_hover_text("Edit the joints it moves").clicked() {
                if open {
                    app.asm.anim.open.retain(|x| *x != m.id);
                } else {
                    app.asm.anim.open.push(m.id);
                }
            }
            e.track(&ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(84.0)));
            let follows = driven.contains(&m.id);
            ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(40.0);
            let r = ui.add_enabled(!follows, egui::Slider::new(&mut m.value, 0.0..=1.0).show_value(false));
            let r = if follows { r.on_disabled_hover_text("Another motion drives this one") } else { r };
            if e.track(&r) {
                e.sliders = true;
                app.asm.anim.playing = false;
                touch(&mut app.asm.anim, m.id);
            }
            ui.label(RichText::new(format!("{:.0}%", m.value * 100.0)).size(11.5).monospace());
            if x_button(ui, "Delete this motion") {
                remove = Some(k);
            }
        });
        if open {
            links_ui(ui, &a, m, &mut e);
        }
    }
    if let Some(k) = remove {
        let id = motions.remove(k).id;
        for m in &mut motions {
            m.links.retain(|l| l.target != Target::Motion(id));
        }
        for an in &mut anims {
            an.clips.retain(|c| c.motion != id);
        }
        e.click();
    }
    let movable = asm_anim::movable_mates(&a);
    ui.add_enabled_ui(!movable.is_empty(), |ui| {
        let r = ui.menu_button("+ Motion", |ui| {
            for (id, name, _) in &movable {
                if ui.button(name).clicked() {
                    let link = asm_anim::new_link(&a, Target::Mate(*id));
                    let mid = next_id();
                    motions.push(Motion { id: mid, name: name.clone(), links: vec![link], value: 0.0 });
                    app.asm.anim.open.push(mid);
                    e.click();
                    e.sliders = true;
                    ui.close();
                }
            }
        });
        r.response.on_hover_text("A slider for one of the mates (add more joints to it after)").on_disabled_hover_text("Add a slider, revolute or cylindrical mate first");
    });

    // ------------------------------------------------ animations
    ui.add_space(8.0);
    ui.label(RichText::new("TIMELINE").color(theme::muted()).size(11.0).strong());
    ui.add_space(2.0);
    let st = &mut app.asm.anim;
    if st.anim.map(|id| !anims.iter().any(|x| x.id == id)).unwrap_or(true) {
        st.anim = anims.first().map(|x| x.id);
    }
    ui.horizontal(|ui| {
        if !anims.is_empty() {
            let cur = anims.iter().find(|x| Some(x.id) == st.anim).map(|x| x.name.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("asm_anim_pick").width(120.0).selected_text(cur).show_ui(ui, |ui| {
                for an in &anims {
                    if ui.selectable_label(Some(an.id) == st.anim, &an.name).clicked() {
                        st.anim = Some(an.id);
                        st.t = 0.0;
                        st.playing = false;
                        e.timeline = true;
                    }
                }
            });
        }
        if ui.add_enabled(!motions.is_empty(), egui::Button::new("+ Animation")).on_disabled_hover_text("Add a motion first").clicked() {
            let id = next_id();
            let clips = motions.first().map(|m| vec![Clip { motion: m.id, start: 0.0, length: 2.0, from: 0.0, to: 1.0, ease: Ease::Smooth }]).unwrap_or_default();
            anims.push(Animation { id, name: format!("Animation {}", anims.len() + 1), clips, repeat: Repeat::Once, autoplay: false });
            st.anim = Some(id);
            st.t = 0.0;
            st.playing = false;
            e.click();
            e.timeline = true;
        }
    });
    let sel = st.anim.and_then(|id| anims.iter().position(|x| x.id == id));
    if let Some(k) = sel {
        let mut delete = false;
        {
            let an = &mut anims[k];
            ui.horizontal(|ui| {
                e.track(&ui.add(egui::TextEdit::singleline(&mut an.name).desired_width(120.0)));
                egui::ComboBox::from_id_salt("asm_anim_repeat").width(110.0).selected_text(an.repeat.label()).show_ui(ui, |ui| {
                    for r in Repeat::ALL {
                        if ui.selectable_label(an.repeat == r, r.label()).clicked() {
                            an.repeat = r;
                            e.click();
                        }
                    }
                });
                if x_button(ui, "Delete this animation") {
                    delete = true;
                }
            });
            if e.track(&ui.checkbox(&mut an.autoplay, RichText::new("Play when the web page opens").size(12.0))) {}
        }
        let dur = anims[k].duration();
        // transport
        ui.horizontal(|ui| {
            let (icon, label) = if st.playing { ("pause", "Pause") } else { ("play", "Play") };
            if ui.add(egui::Button::image_and_text(crate::icons::image(icon, 13.0).tint(theme::text()), label)).clicked() {
                if !st.playing {
                    if anims[k].repeat == Repeat::Once && st.t >= dur - 1e-9 {
                        st.t = 0.0;
                    }
                    e.checkpoint = true;
                    st.last = None;
                }
                st.playing = !st.playing;
            }
            if ui.button("⏮").on_hover_text("Back to the start").clicked() {
                st.t = 0.0;
                st.playing = false;
                e.timeline = true;
            }
            let shown = asm_anim::fold_time(&anims[k], st.t);
            ui.label(RichText::new(format!("{shown:.2} / {dur:.2} s")).size(11.5).monospace());
            egui::ComboBox::from_id_salt("asm_anim_speed").width(54.0).selected_text(format!("{}×", st.speed)).show_ui(ui, |ui| {
                for s in [0.25, 0.5, 1.0, 2.0, 4.0] {
                    ui.selectable_value(&mut st.speed, s, format!("{s}×"));
                }
            });
        });
        timeline(ui, &a, &mut anims[k], st, &mut e);
        // clips
        let mut drop = None;
        for (ci, c) in anims[k].clips.iter_mut().enumerate() {
            ui.push_id(("clip", ci), |ui| {
                ui.horizontal(|ui| {
                    let name = motions.iter().find(|m| m.id == c.motion).map(|m| m.name.clone()).unwrap_or_else(|| "(deleted)".into());
                    egui::ComboBox::from_id_salt("m").width(84.0).selected_text(name).show_ui(ui, |ui| {
                        for m in &motions {
                            if ui.selectable_label(m.id == c.motion, &m.name).clicked() {
                                c.motion = m.id;
                                e.click();
                                e.timeline = true;
                            }
                        }
                    });
                    ui.label(small("at"));
                    if e.track(&ui.add(egui::DragValue::new(&mut c.start).speed(0.05).range(0.0..=3600.0).max_decimals(2).suffix(" s"))) {
                        e.timeline = true;
                    }
                    ui.label(small("for"));
                    if e.track(&ui.add(egui::DragValue::new(&mut c.length).speed(0.05).range(0.0..=3600.0).max_decimals(2).suffix(" s"))) {
                        e.timeline = true;
                    }
                    if x_button(ui, "Remove this clip") {
                        drop = Some(ci);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(small("from"));
                    if e.track(&pct(ui, &mut c.from)) {
                        e.timeline = true;
                    }
                    ui.label(small("to"));
                    if e.track(&pct(ui, &mut c.to)) {
                        e.timeline = true;
                    }
                    ease_combo(ui, &mut c.ease, &mut e);
                });
            });
        }
        if let Some(ci) = drop {
            anims[k].clips.remove(ci);
            e.click();
            e.timeline = true;
        }
        let r = ui.menu_button("+ Clip", |ui| {
            for m in &motions {
                if ui.button(&m.name).clicked() {
                    let an = &mut anims[k];
                    // carry on from where this motion's last clip left it, and go back the other way
                    let from = an.clips.iter().filter(|c| c.motion == m.id).max_by(|p, q| p.end().total_cmp(&q.end())).map(|c| c.to).unwrap_or(m.value);
                    let to = if from > 0.5 { 0.0 } else { 1.0 };
                    an.clips.push(Clip { motion: m.id, start: an.duration(), length: 2.0, from, to, ease: Ease::Smooth });
                    e.click();
                    e.timeline = true;
                    ui.close();
                }
            }
        });
        r.response.on_hover_text("Run a motion after the last clip (drag clips along the timeline to overlap them)");
        if delete {
            anims.remove(k);
            st.anim = anims.first().map(|x| x.id);
            st.playing = false;
            e.click();
        }
    } else if !motions.is_empty() {
        ui.label(small("An animation runs motions one after another (or together) on a timeline, and plays in the exported web page too."));
    }

    // ------------------------------------------------ write back, pose
    if e.changed {
        if e.checkpoint {
            checkpoint(app);
        }
        let asm = &mut app.ws.assemblies[ai];
        asm.next_id = asm.next_id.max(a.next_id + new_ids);
        asm.motions = motions;
        asm.animations = anims;
    } else if e.checkpoint {
        checkpoint(app);
    }
    if e.sliders {
        pose_sliders(app, ai);
    } else if e.timeline && !app.asm.anim.playing {
        if let Some(id) = app.asm.anim.anim {
            let t = app.asm.anim.t;
            pose_at(app, ai, id, t);
        }
    }
}

fn ease_combo(ui: &mut Ui, ease: &mut Ease, e: &mut Edit) {
    egui::ComboBox::from_id_salt("ease").width(70.0).selected_text(ease.label()).show_ui(ui, |ui| {
        for x in Ease::ALL {
            if ui.selectable_label(*ease == x, x.label()).clicked() {
                *ease = x;
                e.click();
                e.sliders = true;
            }
        }
    });
}

/// The joints one motion moves.
fn links_ui(ui: &mut Ui, a: &Assembly, m: &mut Motion, e: &mut Edit) {
    let all = targets(a, m.id);
    let mut drop = None;
    egui::Frame::new().inner_margin(egui::Margin { left: 14, right: 0, top: 2, bottom: 6 }).show(ui, |ui| {
        for (li, l) in m.links.iter_mut().enumerate() {
            ui.push_id(("link", m.id, li), |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("t").width(150.0).selected_text(target_label(a, l.target)).show_ui(ui, |ui| {
                        for &t in &all {
                            if ui.selectable_label(l.target == t, target_label(a, t)).clicked() && l.target != t {
                                *l = Link { start: l.start, end: l.end, ease: l.ease, ..asm_anim::new_link(a, t) };
                                e.click();
                                e.sliders = true;
                            }
                        }
                    });
                    if x_button(ui, "Stop moving this joint") {
                        drop = Some(li);
                    }
                });
                let unit = asm_anim::unit(a, l.target);
                let now = asm_anim::current_value(a, l.target);
                let value = |ui: &mut Ui, v: &mut f64, e: &mut Edit| {
                    let r = if unit == "%" {
                        pct(ui, v)
                    } else {
                        ui.add(egui::DragValue::new(v).speed(if unit == "°" { 1.0 } else { 0.5 }).max_decimals(2).suffix(format!(" {unit}")))
                    };
                    if e.track(&r) {
                        e.sliders = true;
                    }
                    if let Some(n) = now {
                        if ui.small_button("now").on_hover_text("Where the joint is now (move the part there first)").clicked() {
                            *v = n;
                            e.click();
                            e.sliders = true;
                        }
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(small("from"));
                    value(ui, &mut l.from, e);
                    ui.label(small("to"));
                    value(ui, &mut l.to, e);
                });
                ui.horizontal(|ui| {
                    ui.label(small("while the slider goes"));
                    if e.track(&pct(ui, &mut l.start)) {
                        e.sliders = true;
                    }
                    ui.label(small("–"));
                    if e.track(&pct(ui, &mut l.end)) {
                        e.sliders = true;
                    }
                    ease_combo(ui, &mut l.ease, e);
                });
            });
        }
        if let Some(li) = drop {
            m.links.remove(li);
            e.click();
            e.sliders = true;
        }
        ui.menu_button("+ Joint", |ui| {
            for &t in &all {
                if ui.button(target_label(a, t)).clicked() {
                    m.links.push(asm_anim::new_link(a, t));
                    e.click();
                    e.sliders = true;
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Move another joint (or drive another motion) with this slider; set its span to move it before or after the others");
    });
}


/// The clips as bars on a time axis, with the play head. Click or drag to scrub; drag a bar to move its clip.
fn timeline(ui: &mut Ui, a: &Assembly, an: &mut Animation, st: &mut AnimUi, e: &mut Edit) {
    let mut rows: Vec<u64> = vec![];
    for c in &an.clips {
        if !rows.contains(&c.motion) {
            rows.push(c.motion);
        }
    }
    let row_h = 16.0;
    let head = 14.0;
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, head + row_h * rows.len().max(1) as f32 + 4.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, theme::panel_2());
    let span = (an.duration() * 1.08).max(1.0);
    let x_of = |t: f64| rect.left() + 4.0 + ((t / span) as f32) * (rect.width() - 8.0);
    let t_of = |x: f32| (((x - rect.left() - 4.0) / (rect.width() - 8.0)) as f64 * span).max(0.0);
    // ticks: about 6 across
    let step = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0, 120.0].into_iter().find(|s| span / s <= 8.0).unwrap_or(300.0);
    let mut t = 0.0;
    while t <= span + 1e-9 {
        let x = x_of(t);
        p.line_segment([egui::pos2(x, rect.top() + head - 4.0), egui::pos2(x, rect.bottom())], Stroke::new(1.0, theme::border()));
        p.text(egui::pos2(x + 2.0, rect.top() + 1.0), Align2::LEFT_TOP, format!("{t}"), FontId::proportional(9.5), theme::muted());
        t += step;
    }
    let mut bars = vec![];
    for (ci, c) in an.clips.iter().enumerate() {
        let row = rows.iter().position(|r| *r == c.motion).unwrap_or(0);
        let y = rect.top() + head + row as f32 * row_h;
        let r = egui::Rect::from_min_max(egui::pos2(x_of(c.start), y + 1.0), egui::pos2(x_of(c.end()).max(x_of(c.start) + 3.0), y + row_h - 1.0));
        let col = theme::accent();
        p.rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 70));
        p.rect_stroke(r, 3.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
        let name = a.motions.iter().find(|m| m.id == c.motion).map(|m| m.name.as_str()).unwrap_or("?");
        let arrow = if c.to >= c.from { "→" } else { "←" };
        p.with_clip_rect(r.intersect(rect)).text(r.left_center() + egui::vec2(4.0, 0.0), Align2::LEFT_CENTER, format!("{name} {arrow}"), FontId::proportional(10.5), theme::text());
        bars.push((ci, r));
    }
    let x = x_of(asm_anim::fold_time(an, st.t));
    p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(2.0, theme::error()));

    let pointer = resp.interact_pointer_pos();
    if resp.drag_started() {
        st.drag_clip = pointer.and_then(|q| bars.iter().find(|(_, r)| r.contains(q))).map(|(ci, _)| (*ci, t_of(pointer.unwrap().x) - an.clips[*ci].start));
        if st.drag_clip.is_some() {
            e.checkpoint = true;
        }
    }
    match (st.drag_clip, pointer) {
        (Some((ci, off)), Some(q)) if resp.dragged() => {
            if let Some(c) = an.clips.get_mut(ci) {
                let s = ((t_of(q.x) - off).max(0.0) * 20.0).round() / 20.0;
                if (s - c.start).abs() > 1e-9 {
                    c.start = s;
                    e.changed = true;
                    e.timeline = true;
                }
            }
        }
        (None, Some(q)) if resp.dragged() || resp.clicked() => {
            st.t = t_of(q.x).min(an.duration());
            st.playing = false;
            e.timeline = true;
        }
        _ => {}
    }
    if resp.drag_stopped() {
        st.drag_clip = None;
    }
    if bars.is_empty() {
        p.text(rect.center(), Align2::CENTER_CENTER, "No clips yet", FontId::proportional(11.0), theme::muted());
    }
}
