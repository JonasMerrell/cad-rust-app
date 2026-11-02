//! The PCB tab's Simulate view: the schematic with probes and live values, the simulation setup
//! in the sidebar, and the waveform plot under the schematic.

use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui};

use crate::app::{App, ToastKind};
use crate::cam_ui::{row, section};
use crate::circuit::{self, Kind, Sig, SimOut, Source, si};
use crate::circuit_plot::{self, COLORS};
use crate::pcb::{self, Netlist, Pcb};
use crate::pcb_lib::P;
use crate::pcb_ui::{self, Xf};
use crate::theme;

#[derive(Default)]
pub struct SimUi {
    pub out: Option<SimOut>,
    pub err: Option<String>,
    /// The PCB tab and its state when the results were made.
    ran: Option<(usize, Pcb)>,
    /// Plot x range (zoomed), and the x under the cursor.
    xr: Option<(f64, f64)>,
    pub cursor: Option<f64>,
    ms: f64,
}

impl SimUi {
    /// The name of the PCB tab the results are from.
    pub fn ran_name(&self) -> Option<String> {
        self.ran.as_ref().map(|(_, p)| p.name.clone())
    }
}

fn index(app: &App) -> Option<usize> {
    match app.tab {
        crate::workspace::Tab::Pcb(i) if i < app.ws.pcbs.len() => Some(i),
        _ => None,
    }
}

/// Results that belong to the open PCB tab.
fn results(app: &App) -> Option<&SimOut> {
    let i = index(app)?;
    match &app.pcb.sim.ran {
        Some((pi, _)) if *pi == i => app.pcb.sim.out.as_ref(),
        _ => None,
    }
}

fn stale(app: &App) -> bool {
    let Some(i) = index(app) else { return false };
    match &app.pcb.sim.ran {
        Some((pi, p)) => {
            *pi == i && (p.parts != app.ws.pcbs[i].parts || p.wires != app.ws.pcbs[i].wires || p.labels != app.ws.pcbs[i].labels || strip(&p.sim) != strip(&app.ws.pcbs[i].sim))
        }
        None => false,
    }
}

/// The setup without the probes (adding a probe needs no new run).
fn strip(s: &circuit::SimSetup) -> circuit::SimSetup {
    circuit::SimSetup { probes: vec![], ..s.clone() }
}

pub fn run(app: &mut App) {
    let Some(i) = index(app) else { return };
    let p = app.ws.pcbs[i].clone();
    // firmware paths are relative to the document
    let mut q = p.clone();
    if let Some(dir) = app.path.as_ref().and_then(|d| d.parent()) {
        for m in &mut q.sim.mcus {
            let f = std::path::Path::new(m.firmware.trim());
            if !m.firmware.trim().is_empty() && f.is_relative() {
                m.firmware = dir.join(f).to_string_lossy().into_owned();
            }
        }
    }
    let t0 = Instant::now();
    let r = circuit::run_with(&q, &q.sim.analysis, &app.ws.assemblies);
    app.pcb.sim.ms = t0.elapsed().as_secs_f64() * 1e3;
    app.pcb.sim.xr = None;
    app.pcb.sim.cursor = None;
    match r {
        Ok(out) => {
            app.pcb.sim.out = Some(out);
            app.pcb.sim.err = None;
        }
        Err(e) => {
            app.pcb.sim.out = None;
            app.pcb.sim.err = Some(e.clone());
            app.flash(format!("Simulation failed: {e}"), ToastKind::Error);
        }
    }
    app.pcb.sim.ran = Some((i, p));
    if app.pcb.view != pcb_ui::View::Simulate {
        pcb_ui::set_view(app, pcb_ui::View::Simulate);
    }
}

fn set_kind(app: &mut App, k: Kind) {
    pcb_ui::edit(app, |p| p.sim.analysis.kind = k);
}

pub fn toolbar(app: &mut App, ui: &mut Ui) {
    let Some(i) = index(app) else { return };
    let k = app.ws.pcbs[i].sim.analysis.kind;
    App::group(ui, |ui| {
        if app.tool_button(ui, "simDc", "DC", "DC operating point: every net's voltage and every part's current and power", true, k == Kind::Op, true).clicked() {
            set_kind(app, Kind::Op);
        }
        if app.tool_button(ui, "simTran", "Transient", "Voltages and currents over time, from power-on", true, k == Kind::Tran, true).clicked() {
            set_kind(app, Kind::Tran);
        }
        if app.tool_button(ui, "simAc", "AC", "Frequency response (gain and phase) from a source with an AC magnitude", true, k == Kind::Ac, true).clicked() {
            set_kind(app, Kind::Ac);
        }
    });
    App::group(ui, |ui| {
        if app.tool_button(ui, "play", "Run", "Run the simulation (Enter)", true, false, true).clicked() {
            run(app);
        }
        let has = results(app).is_some_and(|o| o.kind != Kind::Op);
        if app.tool_button(ui, "export", "CSV", "Save the probed waveforms as CSV", false, false, has).clicked() {
            save_csv(app);
        }
    });
}

fn save_csv(app: &mut App) {
    let Some(i) = index(app) else { return };
    let Some(out) = results(app) else { return };
    let probes = probe_sigs(out, &app.ws.pcbs[i].sim.probes);
    let text = circuit::csv(out, &probes);
    let name = format!("{}-sim.csv", app.ws.pcbs[i].name.replace(['/', '\\'], "_"));
    let Some(path) = app.save_file_dialog("CSV", &["csv"], name) else { return };
    match std::fs::write(&path, text) {
        Ok(()) => app.flash_info(format!("Saved {}", path.display()), Some(Duration::from_secs(4))),
        Err(e) => app.flash(format!("Could not save: {e}"), ToastKind::Error),
    }
}

/// A result value for display.
fn sv(v: f64, unit: &str) -> String {
    si(circuit::tidy(v, unit), unit)
}

fn probe_sigs(out: &SimOut, probes: &[String]) -> Vec<Sig> {
    probes.iter().filter_map(|q| out.probe(q)).collect()
}

pub fn hint(app: &App) -> String {
    // the plot covers the bottom of the view and has its own hint
    if results(app).is_some_and(|o| o.kind != Kind::Op && o.x.len() > 1) {
        return String::new();
    }
    "Click a net to probe its voltage, a part for its current · Run (Enter)".into()
}

pub fn shortcuts(app: &mut App, ctx: &egui::Context) {
    if ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::F5)) {
        run(app);
    }
}

// ---------------------------------------------------------------- sidebar

/// A number field that takes SI prefixes (10m, 4k7); empty is 0 when `auto` names what 0 means.
fn si_edit(ui: &mut Ui, id: &str, v: &mut f64, unit: &str, auto: Option<&str>) {
    let key = ui.make_persistent_id(id);
    let shown = if auto.is_some() && *v == 0.0 { String::new() } else { si(*v, unit).replace(' ', "") };
    let mut text: String = ui.data_mut(|d| d.get_temp::<String>(key)).unwrap_or_else(|| shown.clone());
    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(90.0).hint_text(auto.unwrap_or("")));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, text.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    // only an edit changes the value: the shown text is rounded to three digits
    if (r.lost_focus() || (r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && text != shown {
        if text.trim().is_empty() && auto.is_some() {
            *v = 0.0;
        } else if let Some(x) = circuit::parse_si(&text) {
            *v = x;
        }
        ui.data_mut(|d| d.remove::<String>(key));
    } else if r.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(key));
    }
}

fn net_combo(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, v: &mut String, nets: &[String], empty: &str) {
    egui::ComboBox::from_id_salt(id).width(110.0).selected_text(if v.is_empty() { empty.to_string() } else { v.clone() }).show_ui(ui, |ui| {
        ui.selectable_value(v, String::new(), empty);
        for n in nets {
            ui.selectable_value(v, n.clone(), n);
        }
    });
}

pub fn sidebar(app: &mut App, ui: &mut Ui) {
    let Some(pi) = index(app) else { return };
    let nl = app.pcb.cache.nl.clone();
    let nets: Vec<String> = nl.nets.iter().map(|n| n.name.clone()).collect();
    theme::field_style(ui);
    section(ui, "Analysis", |ui| analysis_ui(app, ui, pi));
    section(ui, "Sources", |ui| sources_ui(app, ui, pi, &nets));
    section(ui, "Probes", |ui| probes_ui(app, ui, pi));
    if app.ws.pcbs[pi].parts.iter().any(|x| crate::circuit_mcu::is_pico(&x.symbol, &x.value, x.footprint().pin_numbers().len())) {
        section(ui, "Microcontrollers", |ui| mcu_ui(app, ui, pi));
    }
    if app.ws.pcbs[pi].parts.iter().any(|x| crate::circuit_mech::role(&x.symbol).map(|r| r != crate::circuit_mech::Role::Switch).unwrap_or(false))
        || !app.ws.pcbs[pi].sim.links.is_empty()
    {
        section(ui, "Mechanism", |ui| mech_ui(app, ui, pi));
    }
    if let Some(out) = results(app).cloned() {
        section(ui, "Results", |ui| results_ui(app, ui, &out));
    } else if let Some(e) = app.pcb.sim.err.clone() {
        section(ui, "Results", |ui| {
            ui.colored_label(theme::error(), e);
        });
    }
    section(ui, "Models", |ui| {
        let p = &mut app.ws.pcbs[pi];
        // linked switches are limit switches: their mate opens and closes them
        let switches: Vec<String> =
            p.parts.iter().filter(|x| x.symbol == "SW" && !p.sim.links.iter().any(|l| l.part.eq_ignore_ascii_case(&x.reference))).map(|x| x.reference.clone()).collect();
        for s in switches {
            let mut on = p.sim.closed.iter().any(|c| c.eq_ignore_ascii_case(&s));
            if ui.checkbox(&mut on, format!("{s} pressed")).changed() {
                p.sim.closed.retain(|c| !c.eq_ignore_ascii_case(&s));
                if on {
                    p.sim.closed.push(s.clone());
                }
            }
        }
        for part in &p.parts {
            let m = circuit::model(part, &p.sim).map(|m| circuit::describe(&m)).unwrap_or_else(|e| e);
            ui.label(RichText::new(format!("{}  {}", part.reference, m)).size(11.5).color(theme::muted()));
        }
        ui.label(
            RichText::new("Models come from each part's value: change the value (e.g. 2N3904, TL072, Blue, AMS1117-3.3) to change the model.").size(11.0).color(theme::muted()),
        );
    });
}

/// Each Pico's firmware and power, and what it did in the last run.
fn mcu_ui(app: &mut App, ui: &mut Ui, pi: usize) {
    let refs: Vec<String> = app.ws.pcbs[pi].parts.iter().filter(|x| crate::circuit_mcu::is_pico(&x.symbol, &x.value, x.footprint().pin_numbers().len())).map(|x| x.reference.clone()).collect();
    let out = results(app).cloned();
    for r in refs {
        let p = &mut app.ws.pcbs[pi];
        if !p.sim.mcus.iter().any(|m| m.part.eq_ignore_ascii_case(&r)) {
            p.sim.mcus.push(crate::circuit_mcu::McuSetup { part: r.clone(), ..Default::default() });
        }
        let k = p.sim.mcus.iter().position(|m| m.part.eq_ignore_ascii_case(&r)).unwrap();
        ui.label(RichText::new(format!("{r}  Raspberry Pi Pico")).size(12.5).strong());
        let mut browse = false;
        row(ui, "Firmware", |ui| {
            let m = &mut app.ws.pcbs[pi].sim.mcus[k];
            ui.add(egui::TextEdit::singleline(&mut m.firmware).desired_width(ui.available_width() - 34.0).hint_text("a .uf2, .elf, .hex or .bin file"));
            browse = ui.button("…").on_hover_text("Choose the firmware file: a UF2 or ELF from the Pico SDK or Arduino").clicked();
        });
        if browse {
            if let Some(f) = app.open_file_dialog("Firmware", &["uf2", "elf", "hex", "bin"]) {
                let rel = app.path.as_ref().and_then(|d| d.parent()).and_then(|d| f.strip_prefix(d).ok()).map(|x| x.to_path_buf());
                app.ws.pcbs[pi].sim.mcus[k].firmware = rel.unwrap_or(f).to_string_lossy().into_owned();
            }
        }
        let m = &mut app.ws.pcbs[pi].sim.mcus[k];
        ui.checkbox(&mut m.usb, "Powered from USB (5 V on VBUS)");
        row(ui, "UART0 input", |ui| {
            ui.add(egui::TextEdit::singleline(&mut m.uart_in).desired_width(ui.available_width()).hint_text("text sent to GP1 at the start (\\n for Enter)"));
        });
        if let Some(mo) = out.as_ref().and_then(|o| o.mcus.iter().find(|x| x.reference.eq_ignore_ascii_case(&r))) {
            if mo.instructions > 0 {
                ui.label(RichText::new(format!("Ran {:.1} M instructions", mo.instructions as f64 / 1e6)).size(11.5).color(theme::muted()));
            }
            for n in &mo.notes {
                ui.label(RichText::new(format!("⚠ {n}")).size(11.5).color(theme::muted()));
            }
            if !mo.uart.is_empty() {
                ui.label(RichText::new("Serial output").size(11.5).color(theme::muted()));
                egui::Frame::new().fill(theme::input_fill()).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt(("uart", &r)).max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (t, u, line) in &mo.uart {
                            let tag = if *u == 1 { " UART1" } else { "" };
                            ui.label(RichText::new(format!("{:>9}{tag}  {line}", si(*t, "s"))).size(11.0).monospace());
                        }
                    });
                });
            }
            if !mo.pins.is_empty() {
                egui::CollapsingHeader::new(RichText::new("Pins at the end").size(11.5)).id_salt(("pins", &r)).show(ui, |ui| {
                    egui::Grid::new(("mcu_pins", &r)).striped(true).show(ui, |ui| {
                        for (l, st) in &mo.pins {
                            ui.label(RichText::new(l).size(11.5));
                            ui.label(RichText::new(st).size(11.5).monospace());
                            ui.end_row();
                        }
                    });
                });
            }
        }
        ui.add_space(4.0);
    }
    ui.label(RichText::new("The RP2040 runs its firmware during a transient, reading the circuit's voltages and driving its pins. GPIO, PWM, ADC, UART, timers, interrupts and both cores are simulated; PIO, DMA, USB, I2C and SPI devices are not.").size(11.0).color(theme::muted()));
}

/// The assembly mates a part can link to: (assembly, mate id, label, slider, limits).
fn linkable_mates(app: &App) -> Vec<(usize, u64, String, bool, Option<[f64; 2]>)> {
    use crate::assembly::MateKind;
    let mut v = vec![];
    for (ai, a) in app.ws.assemblies.iter().enumerate() {
        for m in a.mates.iter().filter(|m| matches!(m.kind, MateKind::Revolute | MateKind::Slider | MateKind::Cylindrical) && !m.suppressed) {
            // a cylindrical mate's limits are its slide (mm); what links read on it is the angle
            let limits = if m.kind == MateKind::Cylindrical { None } else { m.limits };
            v.push((ai, m.id, format!("{} › {} ({})", a.name, m.name, m.kind.label().to_lowercase()), m.kind == MateKind::Slider, limits));
        }
    }
    v
}

/// Motors, servos and sensors, and the assembly mates they move or read.
fn mech_ui(app: &mut App, ui: &mut Ui, pi: usize) {
    use crate::circuit_mech::{Role, new_link, role};
    let mates = linkable_mates(app);
    let parts: Vec<(String, Role, String)> = app.ws.pcbs[pi].parts.iter().filter_map(|x| role(&x.symbol).map(|r| (x.reference.clone(), r, x.value.clone()))).collect();
    let out = results(app).cloned();
    if mates.is_empty() {
        ui.label(RichText::new("Add an assembly with a revolute, slider or cylindrical mate to link these parts to the design.").size(11.5).color(theme::muted()));
    }
    for (r, role, value) in parts {
        let p = &mut app.ws.pcbs[pi];
        let k = p.sim.links.iter().position(|l| l.part.eq_ignore_ascii_case(&r));
        let desc = match role {
            Role::Motor => crate::circuit_mech::motor_model(&value).name,
            Role::Stepper => crate::circuit_mech::stepper_model(&value).name,
            Role::Servo => crate::circuit_mech::servo_model(&value).name,
            _ => role.label().to_string(),
        };
        // a push button stays a button unless it is linked
        if role == Role::Switch && k.is_none() && mates.is_empty() {
            continue;
        }
        ui.label(RichText::new(format!("{r}  {desc}")).size(12.5).strong());
        let cur = k.map(|k| (p.sim.links[k].assembly, p.sim.links[k].mate));
        let cur_label = cur.and_then(|c| mates.iter().find(|m| (m.0, m.1) == c).map(|m| m.2.clone())).unwrap_or_else(|| if cur.is_some() { "missing mate".into() } else { "Not linked".into() });
        let mut pick: Option<Option<usize>> = None;
        row(ui, if role.drives() { "Drives" } else { "Reads" }, |ui| {
            egui::ComboBox::from_id_salt(("mech_link", &r)).width(ui.available_width()).truncate().selected_text(RichText::new(cur_label).size(12.0)).show_ui(ui, |ui| {
                if ui.selectable_label(cur.is_none(), "Not linked").clicked() {
                    pick = Some(None);
                }
                for (i, m) in mates.iter().enumerate() {
                    if ui.selectable_label(cur == Some((m.0, m.1)), &m.2).clicked() {
                        pick = Some(Some(i));
                    }
                }
            });
        });
        match pick {
            Some(None) => p.sim.links.retain(|l| !l.part.eq_ignore_ascii_case(&r)),
            Some(Some(i)) => {
                let m = &mates[i];
                let l = new_link(&r, role, m.0, m.1, m.3, m.4);
                match k {
                    Some(k) => {
                        // keep the settings that still mean the same on the new mate
                        let old = p.sim.links[k].clone();
                        let same_kind = mates.iter().find(|x| (x.0, x.1) == (old.assembly, old.mate)).map(|x| x.3 == m.3).unwrap_or(false);
                        p.sim.links[k] = if same_kind { crate::circuit_mech::Link { assembly: m.0, mate: m.1, ..old } } else { l };
                    }
                    None => p.sim.links.push(l),
                }
            }
            None => {}
        }
        let Some(k) = p.sim.links.iter().position(|l| l.part.eq_ignore_ascii_case(&r)) else {
            if role == Role::Switch {
                ui.label(RichText::new("A push button (pressed in Models); link it to make it a limit switch").size(11.0).color(theme::muted()));
            }
            ui.add_space(4.0);
            continue;
        };
        let slide = mates.iter().find(|m| (m.0, m.1) == (p.sim.links[k].assembly, p.sim.links[k].mate)).map(|m| m.3).unwrap_or(false);
        let unit = if slide { " mm" } else { "°" };
        let l = &mut p.sim.links[k];
        match role {
            Role::Motor | Role::Stepper => {
                if slide {
                    row(ui, "Lead", |ui| {
                        ui.add(egui::DragValue::new(&mut l.ratio).range(0.01..=1000.0).speed(0.1).suffix(" mm/turn"));
                    });
                } else {
                    row(ui, "Gear ratio", |ui| {
                        ui.add(egui::DragValue::new(&mut l.ratio).range(0.01..=10000.0).speed(0.1).prefix("1:"))
                            .on_hover_text("Motor turns per turn of the mate, after the gearbox in the motor's value (1:48)");
                    });
                }
                row(ui, "Load inertia", |ui| {
                    ui.add(egui::DragValue::new(&mut l.inertia).range(0.0..=1e3).speed(1e-5).max_decimals(6).suffix(if slide { " kg" } else { " kg·m²" }));
                });
                row(ui, "Friction", |ui| {
                    ui.add(egui::DragValue::new(&mut l.load).range(0.0..=1e4).speed(0.01).max_decimals(4).suffix(if slide { " N" } else { " N·m" }))
                        .on_hover_text("Holds the mate until the motor's torque beats it");
                });
                ui.checkbox(&mut l.reverse, "Turn the other way");
            }
            Role::Servo => {
                let rg = l.range.get_or_insert([-90.0, 90.0]);
                row(ui, "At 1 ms", |ui| {
                    ui.add(egui::DragValue::new(&mut rg[0]).speed(1.0).suffix(unit));
                });
                row(ui, "At 2 ms", |ui| {
                    ui.add(egui::DragValue::new(&mut rg[1]).speed(1.0).suffix(unit));
                });
            }
            Role::Pot | Role::Switch => {
                let rg = l.range.get_or_insert([0.0, 90.0]);
                let (a, b) = if role == Role::Pot { ("Pin 1 end at", "Pin 3 end at") } else { ("Closed from", "to") };
                row(ui, a, |ui| {
                    ui.add(egui::DragValue::new(&mut rg[0]).speed(0.5).suffix(unit));
                });
                row(ui, b, |ui| {
                    ui.add(egui::DragValue::new(&mut rg[1]).speed(0.5).suffix(unit));
                });
            }
            Role::Encoder => {
                row(ui, "Counts", |ui| {
                    ui.add(egui::DragValue::new(&mut l.ratio).range(0.01..=100000.0).speed(1.0).suffix(if slide { " per mm" } else { " per turn" }));
                });
            }
        }
        if let Some(t) = out.as_ref().and_then(|o| o.mech.iter().find(|t| t.reference.eq_ignore_ascii_case(&r))) {
            if let (Some(a), Some(b)) = (t.pos.first(), t.pos.last()) {
                let f = |v: f64| crate::expr::trim(v, 2);
                let mut s = format!("{} {}{unit} to {}{unit}", if role.drives() { "Moved it from" } else { "Read it from" }, f(*a), f(*b));
                if let Some(w) = t.rpm.iter().cloned().reduce(|x, y| if y.abs() > x.abs() { y } else { x }) {
                    s.push_str(&format!(", up to {} rpm", f(w)));
                }
                ui.label(RichText::new(s).size(11.5).color(theme::muted()));
            }
        }
        ui.add_space(4.0);
    }
    // play the motion in the assembly
    if let Some(out) = &out {
        let mut asms: Vec<usize> = out.mates.iter().map(|m| m.assembly).collect();
        asms.dedup();
        for ai in asms {
            let Some(a) = app.ws.assemblies.get(ai) else { continue };
            if ui.button(format!("Play in {}", a.name)).on_hover_text("Show the motion in the assembly").clicked() {
                app.switch_tab(crate::workspace::Tab::Assembly(ai));
                crate::asm_ui::checkpoint(app);
                crate::asm_ui::play_motion(app, ai);
            }
        }
    }
    ui.label(
        RichText::new("Motors turn their mate from the current and the back EMF; servos follow 1–2 ms pulses; potentiometers, limit switches and encoders read their mate. Gear, rack and screw relations carry the motion on, and mate limits stop it.")
            .size(11.0)
            .color(theme::muted()),
    );
}

fn analysis_ui(app: &mut App, ui: &mut Ui, pi: usize) {
    let p = &mut app.ws.pcbs[pi];
    let a = &mut p.sim.analysis;
    ui.horizontal(|ui| {
        for k in [Kind::Op, Kind::Tran, Kind::Ac] {
            ui.selectable_value(
                &mut a.kind,
                k,
                match k {
                    Kind::Op => "DC",
                    Kind::Tran => "Transient",
                    Kind::Ac => "AC",
                },
            );
        }
    });
    match a.kind {
        Kind::Op => {
            ui.label(RichText::new("Capacitors open, inductors shorted: the steady state every source's DC value sets up.").size(11.5).color(theme::muted()));
        }
        Kind::Tran => {
            row(ui, "Stop time", |ui| si_edit(ui, "sim_stop", &mut a.stop, "s", Some("auto")));
            row(ui, "Max step", |ui| si_edit(ui, "sim_step", &mut a.step, "s", Some("auto")));
            ui.checkbox(&mut a.from_op, "Start from the DC operating point")
                .on_hover_text("Otherwise the circuit starts at rest (capacitors empty) and the supplies switch on at 0");
        }
        Kind::Ac => {
            row(ui, "From", |ui| si_edit(ui, "sim_f0", &mut a.f_start, "Hz", None));
            row(ui, "To", |ui| si_edit(ui, "sim_f1", &mut a.f_stop, "Hz", None));
            row(ui, "Points/decade", |ui| {
                ui.add(egui::DragValue::new(&mut a.per_decade).range(1..=200));
            });
            if !p.sim.sources.iter().any(|s| s.ac != 0.0) {
                ui.colored_label(theme::error(), "Give the input source an AC magnitude (1) below.");
            }
        }
    }
    ui.add_space(4.0);
    if ui.add_sized([ui.available_width(), 26.0], egui::Button::new(RichText::new("▶  Run").size(13.0))).clicked() {
        run(app);
    }
}

fn sources_ui(app: &mut App, ui: &mut Ui, pi: usize, nets: &[String]) {
    let p = &mut app.ws.pcbs[pi];
    if p.sim.sources.is_empty() {
        let d = circuit::default_sources(p);
        if d.is_empty() {
            ui.label(RichText::new("No power ports: add a source to drive the circuit.").size(11.5).color(theme::muted()));
        } else {
            ui.label(
                RichText::new(format!("From the power ports: {}", d.iter().map(|s| format!("{} {} V", s.plus, s.wave)).collect::<Vec<_>>().join(", ")))
                    .size(11.5)
                    .color(theme::muted()),
            );
        }
        if ui.button("Edit sources").on_hover_text("Replace the automatic supplies with sources you set").clicked() {
            p.sim.sources = d;
            if p.sim.sources.is_empty() {
                p.sim.sources.push(Source { name: "V1".into(), plus: nets.first().cloned().unwrap_or_default(), wave: "5".into(), ..Default::default() });
            }
        }
    } else {
        let mut del = None;
        let stop = if p.sim.analysis.kind == Kind::Tran { p.sim.analysis.stop } else { 0.0 };
        for (k, s) in p.sim.sources.iter_mut().enumerate() {
            ui.push_id(k, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(60.0));
                    ui.checkbox(&mut s.current, "current").on_hover_text("A current source (out of + into the circuit)");
                    if ui.add(egui::Button::image(crate::icons::image("close", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Remove the source").clicked() {
                        del = Some(k);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("+");
                    net_combo(ui, ("sp", k), &mut s.plus, nets, "—");
                    ui.label("−");
                    net_combo(ui, ("sm", k), &mut s.minus, nets, "ground");
                });
                crate::circuit_wave_ui::editor(ui, k, s, stop);
                ui.add_space(4.0);
            });
        }
        if let Some(k) = del {
            p.sim.sources.remove(k);
        }
        ui.horizontal(|ui| {
            if ui.button("Add source").clicked() {
                let n = p.sim.sources.len() + 1;
                p.sim.sources.push(Source {
                    name: format!("V{n}"),
                    plus: nets.first().cloned().unwrap_or_default(),
                    wave: "PULSE(0 5 0 1u 1u 0.5m 1m)".into(),
                    ..Default::default()
                });
            }
            if ui.button("Automatic").on_hover_text("Go back to a DC supply on every power port").clicked() {
                p.sim.sources.clear();
            }
        });
    }
    ui.add_space(2.0);
    let gnd_auto = if p.sim.ground.is_empty() { "auto (GND)" } else { "auto" };
    row(ui, "Ground", |ui| net_combo(ui, "sim_gnd", &mut p.sim.ground, nets, gnd_auto));
}

fn probes_ui(app: &mut App, ui: &mut Ui, pi: usize) {
    let names: Vec<String> = results(app).map(|o| o.names()).unwrap_or_default();
    let p = &mut app.ws.pcbs[pi];
    if p.sim.probes.is_empty() {
        ui.label(RichText::new("Click nets or parts on the schematic to plot them.").size(11.5).color(theme::muted()));
    }
    let mut del = None;
    for (k, q) in p.sim.probes.iter().enumerate() {
        ui.horizontal(|ui| {
            let c = COLORS[k % COLORS.len()];
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
            ui.painter().rect_filled(r.shrink(3.0), 2.0, Color32::from_rgb(c[0], c[1], c[2]));
            ui.label(q);
            if ui.add(egui::Button::image(crate::icons::image("close", 12.0).tint(theme::muted())).frame(false)).on_hover_text("Remove the probe").clicked() {
                del = Some(k);
            }
        });
    }
    if let Some(k) = del {
        p.sim.probes.remove(k);
    }
    if !names.is_empty() {
        let mut pick = String::new();
        egui::ComboBox::from_id_salt("sim_add_probe").width(ui.available_width() - 8.0).selected_text("Add a signal…").show_ui(ui, |ui| {
            for n in &names {
                ui.selectable_value(&mut pick, n.clone(), n);
            }
        });
        if !pick.is_empty() && !p.sim.probes.contains(&pick) {
            p.sim.probes.push(pick);
        }
    }
}

fn results_ui(app: &mut App, ui: &mut Ui, out: &SimOut) {
    let Some(pi) = index(app) else { return };
    if stale(app) {
        ui.colored_label(theme::error(), "The circuit changed: run again to update.");
    }
    ui.label(RichText::new(format!("{} · {} points · {:.0} ms", out.kind.label(), out.x.len(), app.pcb.sim.ms)).size(11.5).color(theme::muted()));
    for n in &out.notes {
        ui.label(RichText::new(format!("⚠ {n}")).size(11.5).color(theme::muted()));
    }
    if out.kind == Kind::Tran && out.stop > 0.0 && app.ws.pcbs[pi].sim.analysis.stop == 0.0 {
        ui.label(RichText::new(format!("Stop time worked out as {}", si(out.stop, "s"))).size(11.5).color(theme::muted()));
    }
    match out.kind {
        Kind::Op => {
            egui::Grid::new("sim_nets").striped(true).show(ui, |ui| {
                for (n, v) in &out.nets {
                    ui.label(RichText::new(n).size(12.0));
                    ui.label(RichText::new(si(*v, "V")).size(12.0).monospace());
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("sim_src").striped(true).show(ui, |ui| {
                for (n, v, i) in &out.sources {
                    ui.label(RichText::new(n).size(12.0));
                    ui.label(RichText::new(format!("{} · {}", sv(*i, "A"), sv(v * i, "W"))).size(12.0).monospace());
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("sim_parts").striped(true).show(ui, |ui| {
                for part in &out.parts {
                    if part.pins.iter().all(|x| x.2 == 0.0) {
                        continue;
                    }
                    ui.label(RichText::new(&part.reference).size(12.0));
                    let i = part.pins.first().map(|x| x.2).unwrap_or(0.0);
                    ui.label(
                        RichText::new(if part.pins.len() == 2 { format!("{} · {}", sv(i.abs(), "A"), sv(part.power, "W")) } else { sv(part.power, "W") }).size(12.0).monospace(),
                    );
                    ui.end_row();
                }
            });
        }
        Kind::Tran => {
            let probes = probe_sigs(out, &app.ws.pcbs[pi].sim.probes);
            for (k, s) in probes.iter().enumerate() {
                let st = circuit::tran_stats(&out.x, &s.re);
                let c = COLORS[k % COLORS.len()];
                ui.label(RichText::new(&s.name).size(12.0).color(Color32::from_rgb(c[0], c[1], c[2])));
                let mut t = format!("{} … {}, average {}", sv(st.min, s.unit), sv(st.max, s.unit), sv(st.avg, s.unit));
                if let (Some(p), Some(d)) = (st.period, st.duty) {
                    t.push_str(&format!("\n{} ({}), duty {:.1} %", si(1.0 / p, "Hz"), si(p, "s"), d * 100.0));
                }
                ui.label(RichText::new(t).size(11.5).monospace());
            }
        }
        _ => {
            let probes = probe_sigs(out, &app.ws.pcbs[pi].sim.probes);
            for (k, s) in probes.iter().enumerate() {
                let st = circuit::ac_stats(&out.x, &s.re, &s.im);
                let c = COLORS[k % COLORS.len()];
                ui.label(RichText::new(&s.name).size(12.0).color(Color32::from_rgb(c[0], c[1], c[2])));
                let z = |v: f64| if v.abs() < 0.05 { 0.0 } else { v };
                let mut t = format!("{:.1} dB at {}, peak {:.1} dB at {}", z(st.start_db), si(out.x[0], "Hz"), z(st.peak_db), si(st.peak_f, "Hz"));
                if let Some(f) = st.f_low {
                    t.push_str(&format!("\n−3 dB low {}", si(f, "Hz")));
                }
                if let Some(f) = st.f_high {
                    t.push_str(&format!("\n−3 dB high {}", si(f, "Hz")));
                }
                ui.label(RichText::new(t).size(11.5).monospace());
            }
        }
    }
}

// ---------------------------------------------------------------- schematic

/// The viewport split into the schematic and, with waveforms to show, the plot below.
pub fn split(app: &App, full: Rect) -> (Rect, Option<Rect>) {
    match results(app) {
        Some(o) if o.kind != Kind::Op && o.x.len() > 1 => {
            let h = (full.height() * 0.45).clamp(150.0, 520.0);
            let (a, b) = full.split_top_bottom_at_y(full.bottom() - h);
            (a, Some(b))
        }
        _ => (full, None),
    }
}

/// The net at a schematic point: on a pin end, a label or a wire.
pub fn net_at(p: &Pcb, nl: &Netlist, at: P, tol: f64) -> Option<String> {
    let pins = p.sch_pins();
    let pin_net = |sp: &pcb::SchPin| nl.net_of(sp.part, &sp.num).map(|n| nl.nets[n].name.clone());
    if let Some(sp) = pins.iter().filter(|x| pcb::dist(x.at, at) < tol * 1.5).min_by(|a, b| pcb::dist(a.at, at).total_cmp(&pcb::dist(b.at, at))) {
        return pin_net(sp);
    }
    if let Some(l) = p.labels.iter().find(|l| pcb::dist(l.at, at) < tol * 1.5) {
        if let Some(sp) = pins.iter().find(|x| pcb::dist(x.at, l.at) < 1e-3) {
            return pin_net(sp);
        }
        return match p.wires.iter().position(|w| pcb::seg_point(w.a, w.b, l.at) < 1e-3) {
            Some(w) => wire_net(p, nl, &pins, w),
            None => nl.by_name(&l.text).map(|n| nl.nets[n].name.clone()),
        };
    }
    let w = p.wires.iter().position(|w| pcb::seg_point(w.a, w.b, at) < tol)?;
    wire_net(p, nl, &pins, w)
}

/// The net of a wire: a pin or label on it or on the wires joined to it.
fn wire_net(p: &Pcb, nl: &Netlist, pins: &[pcb::SchPin], w: usize) -> Option<String> {
    let on = |w: &pcb::Wire, q: P| pcb::seg_point(w.a, w.b, q) < 1e-3;
    let mut seen = vec![false; p.wires.len()];
    let mut stack = vec![w];
    seen[w] = true;
    while let Some(i) = stack.pop() {
        let a = &p.wires[i];
        for sp in pins {
            if on(a, sp.at) {
                return nl.net_of(sp.part, &sp.num).map(|n| nl.nets[n].name.clone());
            }
        }
        for (j, b) in p.wires.iter().enumerate() {
            if !seen[j] && (on(a, b.a) || on(a, b.b) || on(b, a.a) || on(b, a.b)) {
                seen[j] = true;
                stack.push(j);
            }
        }
    }
    p.labels.iter().find(|l| seen.iter().enumerate().any(|(i, s)| *s && on(&p.wires[i], l.at))).and_then(|l| nl.by_name(&l.text)).map(|n| nl.nets[n].name.clone())
}

fn part_at(p: &Pcb, at: P) -> Option<usize> {
    p.parts
        .iter()
        .enumerate()
        .rev()
        .find(|(_, part)| {
            let (lo, hi) = crate::pcb_export::sch_bbox(part);
            at[0] >= lo[0] - 0.5 && at[0] <= hi[0] + 0.5 && at[1] >= lo[1] - 0.5 && at[1] <= hi[1] + 0.5
        })
        .map(|x| x.0)
}

/// The probe a click at `at` toggles.
fn probe_at(p: &Pcb, nl: &Netlist, at: P, tol: f64) -> Option<String> {
    if let Some(n) = net_at(p, nl, at, tol) {
        return Some(format!("V({n})"));
    }
    let i = part_at(p, at)?;
    let part = &p.parts[i];
    let sym = part.symbol();
    if sym.pins.len() == 2 {
        return Some(format!("I({})", part.reference));
    }
    // a multi-pin part: the pin nearest the click
    let pins = p.sch_pins();
    let sp = pins.iter().filter(|x| x.part == i).min_by(|a, b| pcb::dist(a.at, at).total_cmp(&pcb::dist(b.at, at)))?;
    let label = sym.pins.iter().find(|x| x.num == sp.num).map(|x| x.name.clone()).filter(|x| !x.is_empty()).unwrap_or(sp.num.clone());
    Some(format!("I({}.{})", part.reference, label))
}

pub fn sim_input(app: &mut App, pi: usize, hover: Option<P>, tol: f64, clicked: bool) {
    let (Some(h), true) = (hover, clicked) else { return };
    let p = &app.ws.pcbs[pi];
    let Some(q) = probe_at(p, &app.pcb.cache.nl, h, tol) else { return };
    let gnd = results(app).map(|o| o.ground.clone()).unwrap_or_else(|| "GND".into());
    if q.eq_ignore_ascii_case(&format!("V({gnd})")) {
        app.flash_info(format!("{gnd} is the ground (0 V)"), Some(Duration::from_secs(2)));
        return;
    }
    pcb_ui::edit(app, |p| {
        if let Some(k) = p.sim.probes.iter().position(|x| x.eq_ignore_ascii_case(&q)) {
            p.sim.probes.remove(k);
        } else {
            p.sim.probes.push(q.clone());
        }
    });
}

/// A value of a result at the plot cursor (or the end).
fn value_now(out: &SimOut, s: &Sig, cursor: Option<f64>) -> f64 {
    match out.kind {
        Kind::Op => s.re[0],
        Kind::Ac => s.re.first().copied().unwrap_or(0.0),
        Kind::Tran => {
            let t = cursor.unwrap_or(*out.x.last().unwrap_or(&0.0));
            let k = out.x.partition_point(|x| *x < t).min(out.x.len() - 1);
            s.re[k]
        }
    }
}

/// A result for display at the plot cursor: the value (operating point, transient) or, for an
/// AC sweep, the gain and phase at the cursor's frequency (else the first one).
fn value_text(out: &SimOut, s: &Sig, cursor: Option<f64>) -> String {
    if out.kind != Kind::Ac {
        return sv(value_now(out, s, cursor), s.unit);
    }
    if out.x.is_empty() || s.re.is_empty() || s.im.len() != s.re.len() {
        return String::new();
    }
    let f = cursor.unwrap_or(out.x[0]);
    let k = out.x.partition_point(|x| *x < f).min(s.re.len() - 1);
    let (re, im) = (s.re[k], s.im[k]);
    format!("{:.1} dB ∠{:.0}° at {}", circuit::db(re, im), im.atan2(re).to_degrees(), si(out.x[k], "Hz"))
}

/// Where a net's value tag goes: by its first label, else its first pin.
fn tag_spot(p: &Pcb, nl: &Netlist, net: &str) -> Option<P> {
    let ni = nl.by_name(net)?;
    if let Some(l) = p.labels.iter().find(|l| l.text == net) {
        return Some(l.at);
    }
    let (part, num) = nl.nets[ni].pins.first()?;
    p.sch_pins().into_iter().find(|x| x.part == *part && &x.num == num).map(|x| x.at)
}

pub fn overlay(app: &App, pi: usize, painter: &egui::Painter, xf: &Xf) {
    let p = &app.ws.pcbs[pi];
    let nl = &app.pcb.cache.nl;
    let out = results(app);
    let probes = &p.sim.probes;
    let font = egui::FontId::proportional(11.5);
    let tag = |at: P, text: String, col: Color32| {
        let pos = xf.s(at) + egui::vec2(6.0, -14.0);
        let g = painter.layout_no_wrap(text, font.clone(), Color32::WHITE);
        let r = Rect::from_min_size(pos, g.size()).expand2(egui::vec2(4.0, 1.5));
        painter.rect_filled(r, 3.0, col);
        painter.galley(pos, g, Color32::WHITE);
    };
    // live values: every net for an operating point, the probed ones over time
    if let Some(out) = out {
        let opv = out.kind != Kind::Tran;
        for (n, v) in &out.nets {
            let k = probes.iter().position(|q| out.probe(q).is_some_and(|s| s.name.eq_ignore_ascii_case(&format!("V({n})"))));
            if !opv && k.is_none() {
                continue;
            }
            let Some(at) = tag_spot(p, nl, n) else { continue };
            let val = if opv { *v } else { out.probe(&format!("V({n})")).map(|s| value_now(out, &s, app.pcb.sim.cursor)).unwrap_or(0.0) };
            let col = match k {
                Some(k) => {
                    let c = COLORS[k % COLORS.len()];
                    Color32::from_rgb(c[0], c[1], c[2])
                }
                None => Color32::from_rgb(70, 80, 100),
            };
            tag(at, si(val, "V"), col);
        }
    }
    // probe markers on parts
    for (k, q) in probes.iter().enumerate() {
        let c = COLORS[k % COLORS.len()];
        let col = Color32::from_rgb(c[0], c[1], c[2]);
        if let Some(rest) = q.strip_prefix("I(").and_then(|r| r.strip_suffix(')')) {
            let (r, pin) = rest.split_once('.').map(|(a, b)| (a, Some(b))).unwrap_or((rest, None));
            let Some(i) = p.part_index(r) else { continue };
            let part = &p.parts[i];
            let at = match pin {
                Some(pn) => {
                    let sym = part.symbol();
                    let num = sym.pins.iter().find(|x| x.name.eq_ignore_ascii_case(pn) || x.num == pn).map(|x| x.num.clone());
                    num.and_then(|n| p.sch_pins().into_iter().find(|x| x.part == i && x.num == n).map(|x| x.at))
                }
                None => {
                    let (lo, hi) = crate::pcb_export::sch_bbox(part);
                    Some([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0])
                }
            };
            if let Some(at) = at {
                painter.circle_stroke(xf.s(at), 7.0, Stroke::new(2.0, col));
                if let Some(s) = out.and_then(|o| o.probe(q).map(|s| (o, s))) {
                    tag(at, value_text(s.0, &s.1, app.pcb.sim.cursor), col);
                }
            }
        }
    }
    // the hovered net or part
    let h = app.pcb.hover;
    let tol = (6.0 / xf.zoom) as f64;
    if painter.ctx().pointer_hover_pos().is_some_and(|q| xf.rect.contains(q)) {
        if let Some(q) = probe_at(p, nl, h, tol) {
            let mut text = q.clone();
            if let Some(s) = out.and_then(|o| o.probe(&q).map(|s| (o, s))) {
                text = format!("{q} = {}", value_text(s.0, &s.1, app.pcb.sim.cursor));
            }
            if let Some(i) = part_at(p, h).filter(|_| q.starts_with("I(")) {
                let m = circuit::model(&p.parts[i], &p.sim).map(|m| circuit::describe(&m)).unwrap_or_else(|e| e);
                text = format!("{} {}: {m}\n{text}", p.parts[i].reference, p.parts[i].value);
                if let Some(op) = out.filter(|o| o.kind != Kind::Tran).and_then(|o| o.parts.iter().find(|x| x.reference == p.parts[i].reference)) {
                    text.push_str(&format!("\npower {}", sv(op.power, "W")));
                }
            }
            let pos = xf.s(h) + egui::vec2(14.0, 10.0);
            let g = painter.layout(text, font.clone(), theme::text(), 320.0);
            let r = Rect::from_min_size(pos, g.size()).expand(5.0);
            painter.rect_filled(r, 4.0, theme::panel());
            painter.rect_stroke(r, 4.0, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
            painter.galley(pos, g, theme::text());
        }
    }
}

// ---------------------------------------------------------------- plot

pub fn plot(app: &mut App, ui: &mut Ui, rect: Rect) {
    let Some(pi) = index(app) else { return };
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let dark = theme::is_dark();
    painter.rect_filled(rect, 0.0, if dark { Color32::from_rgb(24, 26, 31) } else { Color32::WHITE });
    painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::border()));
    let Some(out) = results(app).cloned() else { return };
    let probes = probe_sigs(&out, &app.ws.pcbs[pi].sim.probes);
    let ink = theme::text();
    let grid = if dark { Color32::from_gray(52) } else { Color32::from_gray(228) };
    if probes.is_empty() {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Click nets or parts on the schematic to plot them", egui::FontId::proportional(13.0), theme::muted());
        return;
    }
    let ac = out.kind == Kind::Ac;
    let full = (out.x[0], *out.x.last().unwrap());
    let (mut x0, mut x1) = app.pcb.sim.xr.unwrap_or(full);
    let area = Rect::from_min_max(rect.min + egui::vec2(70.0, 8.0), rect.max - egui::vec2(14.0, 22.0));
    let fx = |x: f64, x0: f64, x1: f64| area.left() + area.width() * circuit_plot::x_frac(out.kind, x0, x1, x) as f32;
    let xof = |px: f32, x0: f64, x1: f64| {
        let t = ((px - area.left()) / area.width()) as f64;
        if ac { x0 * (x1 / x0).powf(t) } else { x0 + (x1 - x0) * t }
    };
    // navigation
    if let Some(hp) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let c = xof(hp.x, x0, x1);
            let z = (-scroll as f64 * 0.003).exp();
            if ac {
                (x0, x1) = (c * (x0 / c).powf(z), c * (x1 / c).powf(z));
            } else {
                (x0, x1) = (c + (x0 - c) * z, c + (x1 - c) * z);
            }
        }
    }
    if resp.dragged_by(egui::PointerButton::Primary) || resp.dragged_by(egui::PointerButton::Middle) {
        let d = resp.drag_delta().x as f64 / area.width() as f64;
        if ac {
            let k = (x1 / x0).powf(-d);
            (x0, x1) = (x0 * k, x1 * k);
        } else {
            let s = (x1 - x0) * d;
            (x0, x1) = (x0 - s, x1 - s);
        }
    }
    if resp.double_clicked() {
        (x0, x1) = full;
    }
    // stay inside the data
    if ac {
        let r = (x1 / x0).min(full.1 / full.0);
        x0 = x0.max(full.0).min(full.1 / r);
        x1 = x0 * r;
    } else {
        let w = (x1 - x0).min(full.1 - full.0).max((full.1 - full.0) * 1e-6);
        x0 = x0.max(full.0).min(full.1 - w);
        x1 = x0 + w;
    }
    app.pcb.sim.xr = if (x0, x1) == full { None } else { Some((x0, x1)) };
    let cursor = resp.hover_pos().filter(|p| area.x_range().contains(p.x)).map(|p| xof(p.x, x0, x1));
    app.pcb.sim.cursor = cursor;
    painter.text(
        rect.right_top() + egui::vec2(-14.0, 6.0),
        egui::Align2::RIGHT_TOP,
        "Click nets or parts to probe · wheel zooms, drag pans, double-click resets",
        egui::FontId::proportional(11.0),
        theme::muted(),
    );
    let panels = circuit_plot::panels(&out, &probes);
    let n = panels.len() as f32;
    let ph = area.height() / n;
    let font = egui::FontId::proportional(11.0);
    // visible samples
    let k0 = out.x.partition_point(|x| *x < x0).saturating_sub(1);
    let k1 = (out.x.partition_point(|x| *x <= x1) + 1).min(out.x.len());
    for (pk, p) in panels.iter().enumerate() {
        let top = area.top() + ph * pk as f32 + 16.0;
        let bot = area.top() + ph * (pk as f32 + 1.0) - 6.0;
        let box_ = Rect::from_min_max(Pos2::new(area.left(), top), Pos2::new(area.right(), bot));
        // y range over the visible part
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for s in &p.series {
            for y in &s.y[k0..k1] {
                if y.is_finite() {
                    lo = lo.min(*y);
                    hi = hi.max(*y);
                }
            }
        }
        if p.unit == "dB" {
            lo = lo.max(hi - 120.0);
        }
        if !lo.is_finite() {
            (lo, hi) = (p.lo, p.hi);
        }
        let span = (hi - lo).max(hi.abs().max(lo.abs()) * 1e-3).max(1e-12);
        let (lo, hi) = (lo - span * 0.08, hi + span * 0.08);
        let fy = |y: f64| bot - (bot - top) * ((y - lo) / (hi - lo)) as f32;
        for y in circuit_plot::ticks(lo, hi, ((bot - top) / 28.0).max(2.0) as usize) {
            let yy = fy(y);
            painter.hline(box_.x_range(), yy, Stroke::new(1.0, grid));
            painter.text(Pos2::new(area.left() - 6.0, yy), egui::Align2::RIGHT_CENTER, circuit_plot::y_label(p.unit, y), font.clone(), theme::muted());
        }
        let xt = if ac { circuit_plot::log_ticks(x0, x1) } else { circuit_plot::ticks(x0, x1, (area.width() / 90.0) as usize) };
        for x in xt {
            let xx = fx(x, x0, x1);
            painter.vline(xx, box_.y_range(), Stroke::new(1.0, grid));
            if pk + 1 == panels.len() {
                painter.text(Pos2::new(xx, bot + 4.0), egui::Align2::CENTER_TOP, circuit_plot::x_label(out.kind, x), font.clone(), theme::muted());
            }
        }
        painter.rect_stroke(box_, 0.0, Stroke::new(1.0, theme::border()), egui::StrokeKind::Inside);
        let clip = painter.with_clip_rect(box_);
        for s in &p.series {
            let col = Color32::from_rgb(s.color[0], s.color[1], s.color[2]);
            // at most two points per pixel column (its min and max)
            let mut pts: Vec<Pos2> = vec![];
            let mut col_px = f32::NAN;
            let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
            let flush = |pts: &mut Vec<Pos2>, px: f32, mn: f32, mx: f32| {
                if px.is_finite() {
                    pts.push(Pos2::new(px, mn));
                    if mx != mn {
                        pts.push(Pos2::new(px, mx));
                    }
                }
            };
            let dense = (k1 - k0) as f32 > area.width() * 2.0;
            for k in k0..k1 {
                let y = s.y[k];
                if !y.is_finite() {
                    continue;
                }
                let px = fx(out.x[k], x0, x1);
                let py = fy(y);
                if dense {
                    if px.round() != col_px {
                        flush(&mut pts, col_px, mn, mx);
                        col_px = px.round();
                        (mn, mx) = (py, py);
                    } else {
                        mn = mn.min(py);
                        mx = mx.max(py);
                    }
                } else {
                    pts.push(Pos2::new(px, py));
                }
            }
            if dense {
                flush(&mut pts, col_px, mn, mx);
            }
            clip.add(egui::Shape::line(pts, Stroke::new(1.6, col)));
        }
        // legend with the values at the cursor
        let mut lx = area.left() + 4.0;
        for s in &p.series {
            let col = Color32::from_rgb(s.color[0], s.color[1], s.color[2]);
            let mut t = s.name.clone();
            if let Some(cx) = cursor {
                let k = out.x.partition_point(|x| *x < cx).min(out.x.len() - 1);
                t = format!("{t} {}", circuit_plot::y_label(p.unit, s.y[k]));
            }
            painter.line_segment([Pos2::new(lx, top - 8.0), Pos2::new(lx + 12.0, top - 8.0)], Stroke::new(2.5, col));
            let g = painter.layout_no_wrap(t, font.clone(), ink);
            let w = g.size().x;
            painter.galley(Pos2::new(lx + 16.0, top - 15.0), g, ink);
            lx += w + 30.0;
        }
    }
    if let Some(cx) = cursor {
        let xx = fx(cx, x0, x1);
        painter.vline(xx, area.y_range(), Stroke::new(1.0, theme::accent()));
        painter.text(Pos2::new(xx + 4.0, area.bottom() + 4.0), egui::Align2::LEFT_TOP, circuit_plot::x_label(out.kind, cx), font, theme::accent());
        ui.ctx().request_repaint();
    }
}
