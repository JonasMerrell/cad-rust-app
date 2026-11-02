//! A source's waveform editor in the Simulate sidebar: a shape picker with templates, the wave
//! text, a preview you can draw piecewise-linear points on, and CSV import.

use std::sync::Arc;

use egui::{Pos2, RichText, Sense, Stroke, Ui};

use crate::circuit::{Source, si};
use crate::circuit_solve::Wave;
use crate::circuit_wave::{self, num, pwl_text};
use crate::theme;

/// Longer wave text is summarised instead of laid out every frame (an imported scope capture).
const LONG: usize = 600;
/// Up to this many points get handles to drag.
const HANDLES: usize = 400;

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Dc,
    Pulse,
    Sine,
    Pwl,
    Expr,
}

impl Shape {
    const ALL: [Shape; 5] = [Shape::Dc, Shape::Pulse, Shape::Sine, Shape::Pwl, Shape::Expr];

    fn label(self) -> &'static str {
        match self {
            Shape::Dc => "DC",
            Shape::Pulse => "Pulse",
            Shape::Sine => "Sine",
            Shape::Pwl => "Points (PWL)",
            Shape::Expr => "Expression",
        }
    }

    fn of(w: &Wave) -> Shape {
        match w {
            Wave::Dc(_) => Shape::Dc,
            Wave::Pulse { .. } => Shape::Pulse,
            Wave::Sin { .. } => Shape::Sine,
            Wave::Pwl { .. } => Shape::Pwl,
            Wave::Expr { .. } => Shape::Expr,
        }
    }
}

/// The wave text parsed, cached by its text so a long PWL is not parsed every frame.
fn parsed(ui: &Ui, id: egui::Id, text: &str) -> Arc<Result<Wave, String>> {
    let h = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        h.finish()
    };
    let key = id.with("parsed");
    if let Some((hh, w)) = ui.data(|d| d.get_temp::<(u64, Arc<Result<Wave, String>>)>(key))
        && hh == h
    {
        return w;
    }
    let w = Arc::new(Wave::parse(if text.trim().is_empty() { "0" } else { text }));
    ui.data_mut(|d| d.insert_temp(key, (h, w.clone())));
    w
}

/// The level a wave sits at, to carry over into a new shape.
fn level(w: &Wave) -> f64 {
    match w {
        Wave::Dc(v) => *v,
        Wave::Pulse { v2, .. } => *v2,
        Wave::Sin { vo, va, .. } => vo + va,
        _ => w.peak(),
    }
}

fn template(shape: Shape, from: Option<&Wave>, span: f64) -> String {
    let v = from.map(level).filter(|v| *v != 0.0).unwrap_or(5.0);
    let v = num(v);
    match shape {
        Shape::Dc => v,
        Shape::Pulse => format!("PULSE(0 {v} 0 1u 1u 0.5m 1m)"),
        Shape::Sine => format!("SIN(0 {v} 1k)"),
        Shape::Pwl => match from {
            // the current wave as points to edit
            Some(w) if !matches!(w, Wave::Pwl { .. }) => sample_points(w, span),
            _ => format!("PWL(0 0 100u {v} 400u {v} 500u 0 1m 0) R=0"),
        },
        Shape::Expr => format!("{v}*sin(2*pi*1k*t)"),
    }
}

/// A wave turned into PWL points over `span`: its corners, or 64 even samples.
fn sample_points(w: &Wave, span: f64) -> String {
    let mut ts: Vec<f64> = vec![0.0];
    let mut t = 0.0;
    while ts.len() < 200 {
        t = w.next_break(t);
        if !(t < span) {
            break;
        }
        ts.push(t);
    }
    if ts.len() < 3 || matches!(w, Wave::Sin { .. } | Wave::Expr { .. }) {
        ts = (0..=64).map(|k| span * k as f64 / 64.0).collect();
    } else {
        ts.push(span);
    }
    let pts: Vec<(f64, f64)> = ts.iter().map(|t| (*t, w.at(*t))).collect();
    pwl_text(&pts, 0.0, None)
}

/// A time span to preview a wave over when the analysis has no stop time set.
fn natural_span(w: &Wave) -> f64 {
    let s = match w {
        Wave::Dc(_) => 1e-3,
        Wave::Pulse { td, tr, tf, pw, per, .. } => {
            if *per > 0.0 {
                td + 3.0 * per
            } else if pw.is_finite() {
                (td + tr + pw + tf) * 1.5
            } else {
                (td + tr) * 2.0
            }
        }
        Wave::Sin { f, td, .. } if *f > 0.0 => td + 3.0 / f,
        Wave::Sin { .. } => 1e-3,
        Wave::Pwl { pts, td, rep } => {
            let end = pts.last().map(|p| p.0).unwrap_or(0.0);
            td + end + rep.map(|r| 2.0 * (end - r)).unwrap_or(end * 0.1)
        }
        Wave::Expr { .. } => 10e-3,
    };
    if s > 0.0 && s.is_finite() { s } else { 1e-3 }
}

/// Read a CSV file into PWL text, or say why not.
fn import_csv(path: &std::path::Path, col: usize) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cols = circuit_wave::csv_columns(&text)?;
    let (pts, note) = circuit_wave::csv_points(&cols, col)?;
    let mut msg = format!("{} points from {}", pts.len(), path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
    if cols.len() > 2 {
        msg += &format!(", column {} of {}", col + 1, cols.len());
    }
    if !note.is_empty() {
        msg += &format!(" ({note})");
    }
    Ok((pwl_text(&pts, 0.0, None), msg))
}

#[derive(Clone)]
struct Drag {
    k: usize,
    /// The plot's value range when the drag started, held so the point does not chase its own axis.
    y: (f64, f64),
}

/// The source's waveform: shape, text, preview and import. `stop` is the analysis stop time (0: auto).
pub fn editor(ui: &mut Ui, k: usize, s: &mut Source, stop: f64) {
    let id = ui.make_persistent_id(("wave_ed", k));
    let w = parsed(ui, id, &s.wave);
    // points to edit show their own first pass and two repeats, other waves the whole run
    let span = match w.as_ref() {
        Ok(x @ Wave::Pwl { pts, .. }) if pts.len() <= HANDLES => natural_span(x),
        Ok(x) if stop <= 0.0 => natural_span(x),
        _ if stop > 0.0 => stop,
        _ => 1e-3,
    };
    let shape = w.as_ref().as_ref().ok().map(Shape::of);

    ui.horizontal(|ui| {
        let mut pick = shape;
        egui::ComboBox::from_id_salt(("wave_shape", k)).width(110.0).selected_text(shape.map(|x| x.label()).unwrap_or("—")).show_ui(ui, |ui| {
            for sh in Shape::ALL {
                ui.selectable_value(&mut pick, Some(sh), sh.label());
            }
        });
        if pick != shape
            && let Some(sh) = pick
        {
            s.wave = template(sh, w.as_ref().as_ref().ok(), span);
        }
        if ui.button("CSV…").on_hover_text("Import a time, value CSV (a scope or generator export) as points").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("CSV or text", &["csv", "txt", "tsv", "dat", "CSV", "TXT"]).pick_file() {
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| circuit_wave::csv_columns(&t)) {
                    Ok(cols) if cols.len() > 2 => {
                        ui.data_mut(|d| d.insert_temp(id.with("csv"), (path, cols.len())));
                    }
                    _ => {
                        let note = match import_csv(&path, 1) {
                            Ok((txt, msg)) => {
                                s.wave = txt;
                                msg
                            }
                            Err(e) => format!("⚠ {e}"),
                        };
                        ui.data_mut(|d| d.insert_temp(id.with("note"), note));
                    }
                }
            }
        }
        ui.label("AC");
        ui.add(egui::DragValue::new(&mut s.ac).speed(0.1)).on_hover_text("Small-signal magnitude for AC sweeps");
    });

    // a CSV with several value columns: which one
    if let Some((path, n)) = ui.data(|d| d.get_temp::<(std::path::PathBuf, usize)>(id.with("csv"))) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Value column").size(11.5));
            let mut col: usize = ui.data(|d| d.get_temp(id.with("col"))).unwrap_or(1);
            egui::ComboBox::from_id_salt(("csv_col", k)).width(50.0).selected_text(format!("{}", col + 1)).show_ui(ui, |ui| {
                for c in 1..n {
                    ui.selectable_value(&mut col, c, format!("{}", c + 1));
                }
            });
            ui.data_mut(|d| d.insert_temp(id.with("col"), col));
            if ui.button("Import").clicked() {
                let note = match import_csv(&path, col) {
                    Ok((txt, msg)) => {
                        s.wave = txt;
                        msg
                    }
                    Err(e) => format!("⚠ {e}"),
                };
                ui.data_mut(|d| {
                    d.insert_temp(id.with("note"), note);
                    d.remove::<(std::path::PathBuf, usize)>(id.with("csv"));
                });
            }
            if ui.button("Cancel").clicked() {
                ui.data_mut(|d| d.remove::<(std::path::PathBuf, usize)>(id.with("csv")));
            }
        });
    }

    // the text, or a summary of a long one
    let show_long = ui.data(|d| d.get_temp::<bool>(id.with("long"))).unwrap_or(false);
    if s.wave.len() > LONG && !show_long {
        ui.horizontal(|ui| {
            let what = match w.as_ref() {
                Ok(Wave::Pwl { pts, .. }) => format!("{} points, {} to {}s", pts.len(), num(pts[0].0), num(pts.last().unwrap().0)),
                _ => format!("{} characters", s.wave.len()),
            };
            ui.label(RichText::new(what).size(11.5).color(theme::muted()));
            if ui.small_button("Show text").clicked() {
                ui.data_mut(|d| d.insert_temp(id.with("long"), true));
            }
        });
    } else {
        let hint = match shape {
            Some(Shape::Expr) => "2*sin(2*pi*50*t)",
            Some(Shape::Pwl) => "PWL(0 0 1m 5) R=0",
            _ => "5 or PULSE(0 5 0 1u 1u 1m 2m)",
        };
        let rows = if s.wave.len() > 40 { 3 } else { 1 };
        ui.add(egui::TextEdit::multiline(&mut s.wave).desired_rows(rows).desired_width(ui.available_width() - 4.0).font(egui::TextStyle::Monospace).hint_text(hint)).on_hover_text(
            "5, DC 5, PULSE(v1 v2 delay rise fall width period), SIN(offset amplitude frequency delay damping), \
             PWL(t1 v1 t2 v2 …) R=repeat-from TD=delay, or any expression of t in seconds: \
             2*sin(2*pi*50*t) + 0.2*noise(5k*t), t < 1m ? 0 : 5, 3.3*sqr(1k*t). \
             Functions: sin cos tan exp ln log10 sqrt abs sign min max pow mod frac floor ceil round limit(x,lo,hi) if(c,a,b) \
             u(x) ramp(x), and shapes with period 1: sqr tri saw pulse(x,duty) noise(x[,seed]). EXPR(…) DT=10u sets its time step.",
        );
        if s.wave.len() > LONG && ui.small_button("Hide text").clicked() {
            ui.data_mut(|d| d.insert_temp(id.with("long"), false));
        }
    }
    // the text may have changed above
    let w = parsed(ui, id, &s.wave);
    match w.as_ref() {
        Err(e) => {
            ui.colored_label(theme::error(), RichText::new(e).size(11.0));
        }
        Ok(Wave::Dc(_)) => {}
        Ok(w) => {
            if let Some(new) = preview(ui, id, w, span) {
                s.wave = new;
            }
        }
    }
    if let Some(n) = ui.data(|d| d.get_temp::<String>(id.with("note"))) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&n).size(11.0).color(if n.starts_with('⚠') { theme::error() } else { theme::muted() }));
            if ui.add(egui::Button::image(crate::icons::image("close", 10.0).tint(theme::muted())).frame(false)).clicked() {
                ui.data_mut(|d| d.remove::<String>(id.with("note")));
            }
        });
    }
}

/// The wave over 0..span. PWL points can be dragged, added by double-clicking and removed by
/// right-clicking; returns the new wave text when they change.
fn preview(ui: &mut Ui, id: egui::Id, w: &Wave, span: f64) -> Option<String> {
    let width = ui.available_width() - 4.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 74.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::input_fill());
    let plot = rect.shrink2(egui::vec2(6.0, 8.0));
    const N: usize = 400;
    // sample the wave, with its corners, so pulses and points draw sharp
    let mut ts: Vec<f64> = (0..=N).map(|k| span * k as f64 / N as f64).collect();
    let mut t = 0.0;
    for _ in 0..2000 {
        t = w.next_break(t);
        if !(t < span) {
            break;
        }
        ts.push(t);
        ts.push(t * (1.0 - 1e-9));
    }
    ts.sort_by(f64::total_cmp);
    let vs: Vec<f64> = ts.iter().map(|t| w.at(*t)).collect();

    let pwl = match w {
        Wave::Pwl { pts, td, rep } if pts.len() <= HANDLES => Some((pts.clone(), *td, *rep)),
        _ => None,
    };
    let drag: Option<Drag> = ui.data(|d| d.get_temp(id.with("drag")));
    let (mut lo, mut hi) = vs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| (l.min(*v), h.max(*v)));
    if let Some((pts, ..)) = &pwl {
        for p in pts {
            lo = lo.min(p.1);
            hi = hi.max(p.1);
        }
    }
    lo = lo.min(0.0);
    hi = hi.max(0.0);
    if !(hi > lo) {
        hi = lo + 1.0;
    }
    let (top, bottom) = (hi, lo);
    let pad = (hi - lo) * 0.08;
    let (lo, hi) = drag.as_ref().map(|d| d.y).unwrap_or((lo - pad, hi + pad));
    let x_of = |t: f64| plot.left() + (t / span) as f32 * plot.width();
    let y_of = |v: f64| plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height();
    // rounded to about a pixel: 1.25 V, 230 µs rather than 1.2487 V, 229.87 µs
    let tidy_v = |v: f64| {
        let step = 10f64.powf(((hi - lo) / 100.0).log10().floor());
        (v / step).round() * step
    };
    let tidy_t = |t: f64| {
        let step = 10f64.powf((span / 500.0).log10().floor());
        (t / step).round() * step
    };
    let t_of = |x: f32| ((x - plot.left()) / plot.width()) as f64 * span;
    let v_of = |y: f32| lo + ((plot.bottom() - y) / plot.height()) as f64 * (hi - lo);

    // zero line and labels
    let muted = theme::muted();
    painter.line_segment([Pos2::new(plot.left(), y_of(0.0)), Pos2::new(plot.right(), y_of(0.0))], Stroke::new(1.0, muted.gamma_multiply(0.4)));
    let font = egui::FontId::proportional(10.0);
    painter.text(Pos2::new(rect.left() + 3.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, si(top, ""), font.clone(), muted);
    if bottom < 0.0 {
        painter.text(Pos2::new(rect.left() + 3.0, y_of(bottom)), egui::Align2::LEFT_CENTER, si(bottom, ""), font.clone(), muted);
    }
    painter.text(Pos2::new(rect.right() - 3.0, rect.bottom() - 2.0), egui::Align2::RIGHT_BOTTOM, si(span, "s"), font.clone(), muted);
    let line: Vec<Pos2> = ts.iter().zip(&vs).map(|(t, v)| Pos2::new(x_of(*t), y_of(*v))).collect();
    painter.add(egui::Shape::line(line, Stroke::new(1.5, theme::accent())));

    let Some((mut pts, td, rep)) = pwl else {
        if let Some(h) = resp.hover_pos() {
            let t = t_of(h.x);
            painter.text(Pos2::new(rect.right() - 3.0, rect.top() + 2.0), egui::Align2::RIGHT_TOP, format!("{}s: {}", num(t), num(w.at(t))), font, muted);
        }
        return None;
    };
    // the points' handles (their first pass, after the delay)
    let handle = |p: &(f64, f64)| Pos2::new(x_of(p.0 + td), y_of(p.1));
    let near =
        |pts: &[(f64, f64)], pos: Pos2| pts.iter().enumerate().map(|(k, p)| (k, handle(p).distance(pos))).filter(|x| x.1 < 7.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
    let hover = resp.hover_pos().and_then(|q| near(&pts, q));
    for (k, p) in pts.iter().enumerate() {
        let c = handle(p);
        if !rect.contains(c) {
            continue;
        }
        let hot = hover == Some(k) || drag.as_ref().is_some_and(|d| d.k == k);
        painter.circle(c, if hot { 4.5 } else { 3.0 }, if hot { theme::accent() } else { theme::pal().panel }, Stroke::new(1.2, theme::accent()));
    }
    if let Some(r) = rep {
        let x = x_of(r + td);
        painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, muted.gamma_multiply(0.5)));
    }
    let mut changed = false;
    // the press, not where the pointer is once it has moved far enough to count as a drag
    if resp.drag_started()
        && let Some(k) = ui.input(|i| i.pointer.press_origin()).and_then(|q| near(&pts, q))
    {
        ui.data_mut(|d| d.insert_temp(id.with("drag"), Drag { k, y: (lo, hi) }));
    }
    if let Some(d) = &drag {
        if let Some(pos) = resp.interact_pointer_pos()
            && resp.dragged()
            && d.k < pts.len()
        {
            let k = d.k;
            // between its neighbours in time; the first point stays at time 0 when it is there
            let t_lo = if k == 0 { 0.0 } else { pts[k - 1].0 };
            let t_hi = pts.get(k + 1).map(|p| p.0).unwrap_or(f64::INFINITY);
            let mut t = (t_of(pos.x) - td).clamp(t_lo, t_hi);
            if k == 0 && pts[0].0 == 0.0 {
                t = 0.0;
            }
            // a tidy value: three significant digits of the plot's range
            let v = tidy_v(v_of(pos.y));
            let t = if k > 0 { tidy_t(t).clamp(t_lo, t_hi) } else { t };
            if (t, v) != pts[k] {
                pts[k] = (t, v);
                changed = true;
            }
        }
        if resp.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
            ui.data_mut(|d| d.remove::<Drag>(id.with("drag")));
        }
    }
    if resp.double_clicked()
        && let Some(pos) = resp.interact_pointer_pos()
        && near(&pts, pos).is_none()
    {
        let t = tidy_t((t_of(pos.x) - td).max(0.0));
        let k = pts.partition_point(|p| p.0 <= t);
        pts.insert(k, (t, tidy_v(v_of(pos.y))));
        changed = true;
    }
    if resp.secondary_clicked()
        && pts.len() > 2
        && let Some(k) = resp.interact_pointer_pos().and_then(|q| near(&pts, q))
    {
        pts.remove(k);
        changed = true;
    }
    let tip = drag.as_ref().map(|d| d.k).or(hover);
    if let Some(k) = tip.filter(|k| *k < pts.len()) {
        painter.text(Pos2::new(rect.right() - 3.0, rect.top() + 2.0), egui::Align2::RIGHT_TOP, format!("{}s: {}", num(pts[k].0 + td), num(pts[k].1)), font, muted);
    } else if resp.hovered() {
        resp.clone().on_hover_text("Drag a point to move it, double-click to add one, right-click one to remove it");
    }
    if !changed {
        return None;
    }
    // a repeat that no longer starts on a corner starts at the nearest one
    let rep = rep.map(|r| pts.iter().map(|p| p.0).filter(|t| *t < pts.last().unwrap().0).min_by(|a, b| (a - r).abs().total_cmp(&(b - r).abs())).unwrap_or(0.0));
    Some(pwl_text(&pts, td, rep))
}
