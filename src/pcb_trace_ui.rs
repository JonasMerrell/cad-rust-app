//! The trace solver in the PCB tab: the board's stackup, a per-net table of current, heating,
//! resistance, voltage drop and impedance, what a selected track does, and a calculator window.

use egui::{RichText, Ui};

use crate::app::App;
use crate::cam_ui::row;
use crate::pcb::{self, Layer};
use crate::pcb_trace::{self as tr, Calc, LineKind, NetReport, StackKind};
use crate::theme;

pub struct TraceUi {
    pub open: bool,
    pub calc: Calc,
    /// Calculator targets: current is the Calc's own; these are the impedances to solve for.
    z_target: f64,
    zdiff_target: f64,
    /// The per-net report, rebuilt after each change to the board.
    report: Option<Vec<NetReport>>,
}

impl Default for TraceUi {
    fn default() -> Self {
        TraceUi { open: false, calc: Calc::default(), z_target: 50.0, zdiff_target: 90.0, report: None }
    }
}

impl TraceUi {
    pub fn invalidate(&mut self) {
        self.report = None;
    }
}

fn index(app: &App) -> Option<usize> {
    match app.tab {
        crate::workspace::Tab::Pcb(i) if i < app.ws.pcbs.len() => Some(i),
        _ => None,
    }
}

fn amps(a: f64) -> String {
    if a >= 1.0 { format!("{a:.2} A") } else { format!("{:.0} mA", a * 1000.0) }
}

fn ohms(r: f64) -> String {
    if !r.is_finite() {
        "–".into()
    } else if r >= 1.0 {
        format!("{r:.2} Ω")
    } else {
        format!("{:.1} mΩ", r * 1000.0)
    }
}

fn volts(v: f64) -> String {
    if v >= 1.0 { format!("{v:.2} V") } else { format!("{:.1} mV", v * 1000.0) }
}

fn small(s: impl Into<String>) -> RichText {
    RichText::new(s.into()).size(12.0)
}

/// Copper weights and dielectric of the board.
pub fn stackup_ui(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    let b = &mut app.ws.pcbs[pi].board;
    if (b.stackup.total() - b.thickness).abs() > 1e-3 || b.stackup.coppers().len() < 2 {
        b.stackup = tr::stack(b);
    }
    let mut set_total = None;
    for l in &mut b.stackup.layers {
        match l.kind {
            StackKind::Copper => row(ui, &l.name, |ui| {
                let before = l.copper_oz;
                egui::ComboBox::from_id_salt(("pcb-cu", l.name.clone())).width(90.0).selected_text(format!("{} oz ({:.0} µm)", l.copper_oz, l.copper_oz * tr::OZ_MM * 1000.0)).show_ui(ui, |ui| {
                    for oz in [0.5, 1.0, 2.0, 3.0] {
                        ui.selectable_value(&mut l.copper_oz, oz, format!("{oz} oz"));
                    }
                });
                if l.copper_oz != before {
                    l.thickness = l.copper_oz * tr::OZ_MM;
                    set_total = Some(());
                }
            }),
            StackKind::Core | StackKind::Prepreg => row(ui, &l.name, |ui| {
                ui.label(small(format!("{:.3} mm", l.thickness))).on_hover_text("Follows the board thickness");
                ui.add(egui::DragValue::new(&mut l.er).range(1.0..=12.0).speed(0.01).prefix("εr ").max_decimals(2)).on_hover_text("Relative permittivity: FR4 is 4.2–4.7, Rogers 4350B 3.66");
            }),
            StackKind::Mask => {}
        }
    }
    if set_total.is_some() {
        // heavier copper keeps the finished thickness: the core gives way
        let t = b.thickness;
        b.stackup.set_total(t);
    }
    row(ui, "Heating", |ui| {
        ui.add(egui::DragValue::new(&mut b.rules.temp_rise).range(1.0..=100.0).speed(0.5).suffix(" °C")).on_hover_text("How much a track may warm up at its net's current: the check widens tracks to keep under it");
    });
}

/// The per-net table.
pub fn nets_ui(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    if app.pcb.trace.report.is_none() {
        let p = &app.ws.pcbs[pi];
        let c = &app.pcb.cache;
        let pads = &c.pads;
        app.pcb.trace.report = Some(tr::nets(p, &c.nl, pads, &|n| tr::target(p, &c.nl, n)));
    }
    let rep = app.pcb.trace.report.clone().unwrap_or_default();
    if rep.is_empty() {
        ui.label(small("Route tracks to see their resistance, heating and impedance. Give nets a current or impedance in a net class to check them.").color(theme::muted()));
    }
    let mut widen: Option<(String, f64)> = None;
    for r in &rep {
        let short = r.current > 0.0 && r.min_width + 1e-6 < r.need_width;
        let off_z = r.impedance > 0.0 && (r.z_min < 0.9 * r.impedance || r.z_max > 1.1 * r.impedance);
        let col = if short { theme::error_soft() } else if off_z { theme::accent() } else { theme::text() };
        let on = app.pcb.highlight.as_deref() == Some(r.net.as_str());
        let head = if r.current > 0.0 { format!("{}  ·  {}", r.net, amps(r.current)) } else { r.net.clone() };
        if ui.add(egui::Button::selectable(on, RichText::new(head).size(12.5).color(col).strong())).on_hover_text("Highlight this net").clicked() {
            app.pcb.highlight = if on { None } else { Some(r.net.clone()) };
        }
        ui.indent(("tr", &r.net), |ui| {
            if r.tracks > 0 {
                let w = if (r.max_width - r.min_width).abs() < 1e-6 { format!("{:.2} mm", r.min_width) } else { format!("{:.2}–{:.2} mm", r.min_width, r.max_width) };
                ui.label(small(format!("{} tracks, {:.1} mm long, {w}{}", r.tracks, r.length, if r.vias > 0 { format!(", {} via{}", r.vias, if r.vias == 1 { "" } else { "s" }) } else { String::new() })).color(theme::muted()));
                if !r.worst.0.is_empty() {
                    let mut s = format!("{} from {} to {}", ohms(r.resistance), r.worst.0, r.worst.1);
                    if r.current > 0.0 {
                        s += &format!(": drops {}", volts(r.drop));
                    }
                    ui.label(small(s)).on_hover_text(if r.poured { "The worst pad-to-pad path through tracks and vias. Pads joined only by the pour are left out" } else { "The worst pad-to-pad path through tracks and vias" });
                }
                if r.open {
                    ui.label(small(if r.poured { "Some pads are joined only by the pour" } else { "Some pads are not joined by tracks yet" }).color(theme::muted()));
                }
                if r.current > 0.0 {
                    let s = format!("Warms {:.1} °C; {} needs {:.2} mm at {} °C", r.rise, amps(r.current), r.need_width, app.ws.pcbs[pi].board.rules.temp_rise);
                    ui.label(small(s).color(if short { theme::error_soft() } else { theme::text() }));
                    if short && ui.small_button("Widen to fit").on_hover_text("Give every track of this net the width its current needs").clicked() {
                        widen = Some((r.net.clone(), r.need_width));
                    }
                }
                let z = if (r.z_max - r.z_min).abs() < 0.5 { format!("{:.0} Ω", r.z_min) } else { format!("{:.0}–{:.0} Ω", r.z_min, r.z_max) };
                let mut s = format!("Impedance {z}");
                if r.impedance > 0.0 {
                    s += &format!(" (target {} Ω{})", r.impedance, if r.z_width > 0.0 { format!(": {:.3} mm", r.z_width) } else { String::new() });
                }
                ui.label(small(s).color(if off_z { theme::accent() } else { theme::text() }));
            } else if r.current > 0.0 {
                ui.label(small(format!("Not routed: {} needs {:.2} mm tracks", amps(r.current), r.need_width)).color(theme::muted()));
            }
        });
        ui.add_space(2.0);
    }
    ui.add_space(4.0);
    if ui.button("Trace calculator…").on_hover_text("Work out width, current, heating, resistance and impedance for any track").clicked() {
        app.pcb.trace.open = true;
    }
    if let Some((net, w)) = widen {
        let base = app.ws.pcbs[pi].board.rules.track;
        let w = ((w.max(base)) / 0.05).ceil() * 0.05;
        crate::pcb_ui::edit(app, |p| {
            for t in p.board.tracks.iter_mut().filter(|t| t.net == net && t.width < w) {
                t.width = w;
            }
        });
        app.flash_info(format!("{net} tracks are now {w:.2} mm: run Check for clearances, or Remove tracks and Autoroute to route them wide"), Some(std::time::Duration::from_secs(5)));
    }
}

/// What a selected track does: resistance, heating and impedance.
pub fn track_info(app: &App, ui: &mut Ui, t: &pcb::Track) {
    let Some(pi) = index(app) else { return };
    let p = &app.ws.pcbs[pi];
    let st = tr::stack(&p.board);
    let th = st.copper_mm(t.layer);
    let len = pcb::dist(t.a, t.b);
    let dt = p.board.rules.temp_rise;
    let tg = tr::target(p, &app.pcb.cache.nl, &t.net);
    let z = st.line(t.layer).line(t.width);
    ui.label(small(format!("{len:.2} mm, {} at 25 °C", ohms(tr::resistance(len, t.width, th, tr::AMBIENT)))).color(theme::muted()));
    ui.label(small(format!("Carries {} at {dt} °C rise ({} oz)", amps(tr::current_for(t.width, th, dt, true)), st.copper_oz(t.layer)))).on_hover_text("IPC-2221, outer layer");
    if tg.current > 0.0 {
        let rise = tr::temp_rise(t.width, th, tg.current, true);
        ui.label(small(format!("At {} it warms {rise:.1} °C", amps(tg.current))).color(if rise > dt { theme::error_soft() } else { theme::text() }));
    }
    ui.label(small(format!("{:.0} Ω microstrip, {:.2} ps/mm", z.z0, z.delay)));
}

/// The calculator window.
pub fn window(app: &mut App, ctx: &egui::Context) {
    if !app.pcb.trace.open {
        return;
    }
    let mut open = true;
    let stack = index(app).map(|i| tr::stack(&app.ws.pcbs[i].board));
    let sel_track = match (index(app), app.pcb.sel) {
        (Some(i), Some(crate::pcb_ui::Sel::Track(k))) => app.ws.pcbs[i].board.tracks.get(k).cloned().map(|t| (k, t)),
        _ => None,
    };
    let mut apply_width = None;
    egui::Window::new("Trace calculator").open(&mut open).resizable(false).default_width(320.0).default_pos([320.0, 110.0]).show(ctx, |ui| {
        theme::field_style(ui);
        let c = &mut app.pcb.trace.calc;
        let num = |ui: &mut Ui, label: &str, v: &mut f64, unit: &str, range: std::ops::RangeInclusive<f64>, speed: f64, tip: &str| {
            row(ui, label, |ui| {
                ui.add(egui::DragValue::new(v).range(range).speed(speed).suffix(unit).max_decimals(3)).on_hover_text(tip);
            });
        };
        ui.label(RichText::new("Track").strong());
        num(ui, "Width", &mut c.width, " mm", 0.01..=50.0, 0.005, "Track width");
        row(ui, "Copper", |ui| {
            egui::ComboBox::from_id_salt("tc-oz").width(70.0).selected_text(format!("{} oz", c.copper_oz)).show_ui(ui, |ui| {
                for oz in [0.5, 1.0, 2.0, 3.0] {
                    ui.selectable_value(&mut c.copper_oz, oz, format!("{oz} oz"));
                }
            });
            ui.selectable_value(&mut c.outer, true, "Outer");
            ui.selectable_value(&mut c.outer, false, "Inner");
        });
        num(ui, "Current", &mut c.current, " A", 0.0..=200.0, 0.01, "What the track carries");
        num(ui, "Length", &mut c.length, " mm", 0.0..=10000.0, 0.5, "For the resistance and voltage drop");
        num(ui, "Max. rise", &mut c.temp_rise, " °C", 1.0..=100.0, 0.5, "Allowed temperature rise over ambient");
        num(ui, "Ambient", &mut c.ambient, " °C", -40.0..=125.0, 0.5, "For the resistance at running temperature");
        ui.add_space(4.0);
        ui.label(RichText::new("Line").strong());
        row(ui, "Kind", |ui| {
            ui.selectable_value(&mut c.geometry.kind, LineKind::Microstrip, "Microstrip").on_hover_text("Outer layer over a plane");
            ui.selectable_value(&mut c.geometry.kind, LineKind::Stripline, "Stripline").on_hover_text("Inner layer between two planes");
        });
        num(ui, "Dielectric", &mut c.geometry.h, " mm", 0.02..=10.0, 0.005, "To the reference plane (stripline: to each plane)");
        if c.geometry.kind == LineKind::Stripline {
            num(ui, "Other side", &mut c.geometry.h2, " mm", 0.0..=10.0, 0.005, "To the second plane; 0 when centred");
        }
        num(ui, "εr", &mut c.geometry.er, "", 1.0..=12.0, 0.01, "Relative permittivity: FR4 4.2–4.7");
        num(ui, "Pair gap", &mut c.gap, " mm", 0.0..=10.0, 0.005, "Edge-to-edge gap of a differential pair; 0 for single ended");
        ui.horizontal(|ui| {
            if let Some(st) = &stack {
                if ui.small_button("From board, top").on_hover_text("Copper and dielectric of this board's top layer").clicked() {
                    let g = st.line(Layer::Top);
                    c.geometry = g;
                    c.copper_oz = st.copper_oz(Layer::Top);
                    c.outer = true;
                }
            }
            if let Some((_, t)) = &sel_track {
                if ui.small_button("From selected track").clicked() {
                    c.width = t.width;
                    c.length = pcb::dist(t.a, t.b);
                    if let Some(st) = &stack {
                        c.geometry = st.line(t.layer);
                        c.copper_oz = st.copper_oz(t.layer);
                    }
                }
            }
        });
        let o = c.solve();
        ui.separator();
        ui.label(RichText::new("Result").strong());
        let res = |ui: &mut Ui, label: &str, v: String| {
            row(ui, label, |ui| {
                ui.label(small(v));
            });
        };
        res(ui, "Carries", format!("{} at {} °C rise", amps(o.max_current), c.temp_rise));
        res(ui, "Warms", format!("{:.1} °C at {}", o.rise, amps(c.current)));
        res(ui, "Needs", format!("{:.3} mm for {}", o.width_for_current, amps(c.current)));
        res(ui, "Resistance", ohms(o.resistance));
        res(ui, "Drop", format!("{}, {:.2} W", volts(o.drop), o.power));
        res(ui, "Impedance", format!("{:.1} Ω, εeff {:.2}, {:.2} ps/mm", o.z0, o.er_eff, o.delay));
        if c.gap > 0.0 {
            res(ui, "Differential", format!("{:.1} Ω", o.zdiff));
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Solve for width").strong());
        let g = tr::Geometry { t: c.t(), ..c.geometry };
        ui.horizontal(|ui| {
            if ui.button("For the current").clicked() {
                c.width = o.width_for_current.max(0.01);
            }
            ui.add(egui::DragValue::new(&mut app.pcb.trace.z_target).range(10.0..=200.0).speed(0.5).suffix(" Ω"));
            if ui.button("For Z₀").clicked() {
                if let Some(w) = g.width_for(app.pcb.trace.z_target) {
                    c.width = w;
                }
            }
        });
        if c.gap > 0.0 {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut app.pcb.trace.zdiff_target).range(20.0..=300.0).speed(0.5).suffix(" Ω"));
                if ui.button("Width for Zdiff").on_hover_text("Keep the gap, change the width").clicked() {
                    if let Some(w) = g.diff_width_for(app.pcb.trace.zdiff_target, c.gap) {
                        c.width = w;
                    }
                }
                if ui.button("Gap for Zdiff").on_hover_text("Keep the width, change the gap").clicked() {
                    if let Some(s) = g.diff_gap_for(app.pcb.trace.zdiff_target, c.width) {
                        c.gap = s;
                    }
                }
            });
        }
        if let Some((k, _)) = &sel_track {
            if ui.button(format!("Set the selected track to {:.3} mm", c.width)).clicked() {
                apply_width = Some((*k, c.width));
            }
        }
        ui.label(small("Current: IPC-2221. Impedance: Hammerstad-Jensen microstrip, Wheeler stripline, IPC-2141 pairs.").color(theme::muted()));
    });
    if let Some((k, w)) = apply_width {
        crate::pcb_ui::edit(app, |p| {
            if let Some(t) = p.board.tracks.get_mut(k) {
                t.width = (w * 1000.0).round() / 1000.0;
            }
        });
    }
    if !open {
        app.pcb.trace.open = false;
    }
}
