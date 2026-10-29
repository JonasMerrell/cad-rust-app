//! Agent tools for simulating a PCB tab's circuit: the setup (sources, ground, switches, probes)
//! and the analyses (DC operating point, transient, AC sweep) with measurements and plots.

use serde_json::{Value, json};

use super::{R, Session};
use crate::circuit::{self, Kind, Sig, SimOut, Source};

/// Four significant digits, for JSON.
fn sig4(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let e = v.abs().log10().floor() as i32;
    let k = 10f64.powi(3 - e);
    (v * k).round() / k
}

fn sources_json(s: &[Source]) -> Value {
    json!(
        s.iter()
            .map(|x| json!({"name": x.name, "plus": x.plus, "minus": if x.minus.is_empty() { "ground" } else { &x.minus }, "wave": x.wave, "current": x.current, "ac": x.ac}))
            .collect::<Vec<_>>()
    )
}

fn parse_sources(v: &Value) -> R<Vec<Source>> {
    let list = v.as_array().ok_or("sources: a list of {name, plus, minus, wave, ac, current}")?;
    let mut out = vec![];
    for (k, s) in list.iter().enumerate() {
        let plus = s.get("plus").and_then(|v| v.as_str()).ok_or_else(|| format!("sources[{k}]: give plus (a net name)"))?;
        let wave = match s.get("wave") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(w)) => w.clone(),
            None => "0".into(),
            _ => return Err(format!("sources[{k}]: wave is a number or text like PULSE(0 5 0 1u 1u 1m 2m)")),
        };
        circuit_wave_ok(&wave).map_err(|e| format!("sources[{k}]: {e}"))?;
        out.push(Source {
            name: s.get("name").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| format!("V{}", k + 1)),
            plus: plus.to_string(),
            minus: s.get("minus").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            wave,
            current: s.get("current").and_then(|v| v.as_bool()).unwrap_or(false),
            ac: s.get("ac").and_then(|v| v.as_f64()).unwrap_or(0.0),
        });
    }
    Ok(out)
}

fn circuit_wave_ok(w: &str) -> R<()> {
    crate::circuit_solve::Wave::parse(w).map(|_| ())
}

fn kind(s: &str) -> R<Kind> {
    match s {
        "op" | "dc" => Ok(Kind::Op),
        "tran" | "transient" => Ok(Kind::Tran),
        "ac" => Ok(Kind::Ac),
        o => Err(format!("analysis is op, tran or ac, not {o}")),
    }
}

/// The probes to report: those asked for, else the setup's, else every net voltage.
fn probe_sigs(out: &SimOut, asked: &[String]) -> R<Vec<Sig>> {
    if asked.is_empty() {
        return Ok(out.sigs.iter().filter(|s| s.unit == "V").take(8).cloned().collect());
    }
    asked.iter().map(|q| out.probe(q).ok_or_else(|| format!("no signal {q}; there are {}", out.names().join(", ")))).collect()
}

impl Session {
    pub(super) fn circuit_call(&mut self, name: &str, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        match name {
            "pcb_sim_setup" => {
                let pi = self.pcb_idx(a)?;
                let mut p = self.ws.pcbs[pi].clone();
                if let Some(s) = a.get("sources") {
                    p.sim.sources = parse_sources(s)?;
                }
                if let Some(g) = a.get("ground").and_then(|v| v.as_str()) {
                    p.sim.ground = g.to_string();
                }
                if let Some(c) = a.get("closed").and_then(|v| v.as_array()) {
                    p.sim.closed = c.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
                if let Some(c) = a.get("probes").and_then(|v| v.as_array()) {
                    p.sim.probes = c.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
                if let Some(ms) = a.get("mcus").and_then(|v| v.as_array()) {
                    for (k, m) in ms.iter().enumerate() {
                        let part = m.get("part").and_then(|v| v.as_str()).ok_or_else(|| format!("mcus[{k}]: give part (the Pico's reference)"))?;
                        if !p.parts.iter().any(|x| x.reference.eq_ignore_ascii_case(part)) {
                            return Err(format!("mcus[{k}]: no part {part}"));
                        }
                        if !p.sim.mcus.iter().any(|x| x.part.eq_ignore_ascii_case(part)) {
                            p.sim.mcus.push(crate::circuit_mcu::McuSetup { part: part.to_string(), ..Default::default() });
                        }
                        let e = p.sim.mcus.iter_mut().find(|x| x.part.eq_ignore_ascii_case(part)).unwrap();
                        if let Some(f) = m.get("firmware").and_then(|v| v.as_str()) {
                            if !f.is_empty() {
                                let b = std::fs::read(f).map_err(|err| format!("mcus[{k}]: can't read {f}: {err}"))?;
                                crate::mcu_rp2040::load_firmware(&b).map_err(|err| format!("mcus[{k}]: {err}"))?;
                            }
                            e.firmware = f.to_string();
                        }
                        if let Some(u) = m.get("usb").and_then(|v| v.as_bool()) {
                            e.usb = u;
                        }
                        if let Some(u) = m.get("uart_in").and_then(|v| v.as_str()) {
                            e.uart_in = u.to_string();
                        }
                    }
                }
                apply_analysis(&mut p.sim.analysis, a)?;
                let b = circuit::build(&p)?;
                self.ws.pcbs[pi] = p;
                let p = &self.ws.pcbs[pi];
                let defaulted = p.sim.sources.is_empty();
                let srcs = if defaulted { circuit::default_sources(p) } else { p.sim.sources.clone() };
                let v = json!({
                    "pcb": pi,
                    "ground": b.ground,
                    "sources": sources_json(&srcs),
                    "sources_from_power_nets": defaulted,
                    "parts": b.parts.iter().map(|x| json!({"reference": x.reference, "model": x.desc, "simulated": x.modelled})).collect::<Vec<_>>(),
                    "signals": b.sigs.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                    "probes": p.sim.probes,
                    "mcus": p.sim.mcus.iter().map(|m| json!({"part": m.part, "firmware": m.firmware, "usb": m.usb, "uart_in": m.uart_in})).collect::<Vec<_>>(),
                    "notes": b.notes,
                });
                Ok((v, None))
            }
            "simulate_pcb" => {
                let pi = self.pcb_idx(a)?;
                let mut setup = self.ws.pcbs[pi].sim.clone();
                if let Some(k) = a.get("analysis").and_then(|v| v.as_str()) {
                    setup.analysis.kind = kind(k)?;
                }
                apply_analysis(&mut setup.analysis, a)?;
                if let Some(c) = a.get("probes").and_then(|v| v.as_array()) {
                    setup.probes = c.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
                // the UI opens on the same analysis
                self.ws.pcbs[pi].sim = setup.clone();
                let out = circuit::run(&self.ws.pcbs[pi], &setup.analysis)?;
                let probes = probe_sigs(&out, &setup.probes)?;
                let mut v = json!({"pcb": pi, "analysis": format!("{:?}", out.kind).to_lowercase(), "notes": out.notes});
                match out.kind {
                    Kind::Op => {
                        v["nets"] = json!(out.nets.iter().map(|(n, x)| (n.clone(), json!(sig4(*x)))).collect::<serde_json::Map<_, _>>());
                        v["sources"] = json!(out.sources.iter().map(|(n, u, i)| json!({"name": n, "volts": sig4(*u), "amps": sig4(*i), "watts": sig4(u * i)})).collect::<Vec<_>>());
                        v["parts"] = json!(out.parts.iter().filter(|x| !x.pins.iter().all(|p| p.2 == 0.0)).map(|x| json!({
                            "reference": x.reference,
                            "model": x.desc,
                            "watts": sig4(circuit::tidy(x.power, "W")),
                            "pins": x.pins.iter().map(|(l, u, i)| (l.clone(), json!({"volts": sig4(*u), "amps": sig4(circuit::tidy(*i, "A"))}))).collect::<serde_json::Map<_, _>>(),
                        })).collect::<Vec<_>>());
                    }
                    Kind::Tran => {
                        v["stop"] = json!(sig4(*out.x.last().unwrap_or(&0.0)));
                        v["points"] = json!(out.x.len());
                        let at: Vec<f64> = a.get("at").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
                        let mut m = serde_json::Map::new();
                        for s in &probes {
                            let st = circuit::tran_stats(&out.x, &s.re);
                            let mut o = json!({"unit": s.unit, "min": sig4(st.min), "max": sig4(st.max), "average": sig4(st.avg), "rms": sig4(st.rms), "final": sig4(st.last)});
                            if let (Some(p), Some(d)) = (st.period, st.duty) {
                                o["period"] = json!(sig4(p));
                                o["frequency"] = json!(sig4(1.0 / p));
                                o["duty"] = json!(sig4(d));
                            }
                            if !at.is_empty() {
                                o["at"] = json!(at.iter().map(|t| sig4(interp(&out.x, &s.re, *t))).collect::<Vec<_>>());
                            }
                            m.insert(s.name.clone(), o);
                        }
                        v["probes"] = Value::Object(m);
                        if !out.mcus.is_empty() {
                            v["mcus"] = json!(out.mcus.iter().map(|m| {
                                let skip = m.uart.len().saturating_sub(200);
                                json!({
                                    "part": m.reference,
                                    "firmware": m.firmware,
                                    "instructions": m.instructions,
                                    "notes": m.notes,
                                    "serial": m.uart[skip..].iter().map(|(t, u, l)| json!({"t": sig4(*t), "uart": u, "text": l})).collect::<Vec<_>>(),
                                    "serial_lines_dropped": skip,
                                    "pins_at_end": m.pins.iter().map(|(l, s)| (l.clone(), json!(s))).collect::<serde_json::Map<_, _>>(),
                                })
                            }).collect::<Vec<_>>());
                        }
                    }
                    Kind::Ac => {
                        let at: Vec<f64> = a.get("at").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
                        let mut m = serde_json::Map::new();
                        for s in &probes {
                            let st = circuit::ac_stats(&out.x, &s.re, &s.im);
                            let mut o = json!({"start_db": sig4(st.start_db), "peak_db": sig4(st.peak_db), "peak_hz": sig4(st.peak_f)});
                            if let Some(f) = st.f_low {
                                o["low_3db_hz"] = json!(sig4(f));
                            }
                            if let Some(f) = st.f_high {
                                o["high_3db_hz"] = json!(sig4(f));
                            }
                            if !at.is_empty() {
                                o["at"] = json!(
                                    at.iter()
                                        .map(|f| {
                                            let (re, im) = (interp_log(&out.x, &s.re, *f), interp_log(&out.x, &s.im, *f));
                                            json!({"hz": f, "db": sig4(circuit::db(re, im)), "phase": sig4(im.atan2(re).to_degrees())})
                                        })
                                        .collect::<Vec<_>>()
                                );
                            }
                            m.insert(s.name.clone(), o);
                        }
                        v["probes"] = Value::Object(m);
                    }
                }
                if let Some(path) = a.get("csv").and_then(|v| v.as_str()) {
                    super::write_out(path, circuit::csv(&out, &probes))?;
                    v["csv"] = json!(path);
                }
                let want_png = a.get("path").is_some() || a.get("plot").and_then(|v| v.as_bool()).unwrap_or(false);
                if want_png && out.kind != Kind::Op {
                    let width = a.get("width").and_then(|v| v.as_u64()).unwrap_or(1000).clamp(200, 4000) as u32;
                    let png = crate::circuit_plot::png(&out, &probes, width)?;
                    if let Some(path) = a.get("path").and_then(|p| p.as_str()) {
                        super::write_out(path, &png)?;
                        v["saved"] = json!(path);
                    }
                    return Ok((v, Some(png)));
                }
                Ok((v, None))
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }
}

fn apply_analysis(an: &mut circuit::Analysis, a: &Value) -> R<()> {
    let num = |k: &str| -> R<Option<f64>> {
        match a.get(k) {
            None => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_f64()),
            Some(Value::String(s)) => circuit::parse_si(s).map(Some).ok_or_else(|| format!("{k}: {s} is not a number")),
            _ => Err(format!("{k}: a number")),
        }
    };
    if let Some(v) = num("stop")? {
        an.stop = v;
    }
    if let Some(v) = num("step")? {
        an.step = v;
    }
    if let Some(v) = num("f_start")? {
        an.f_start = v;
    }
    if let Some(v) = num("f_stop")? {
        an.f_stop = v;
    }
    if let Some(v) = a.get("per_decade").and_then(|v| v.as_u64()) {
        an.per_decade = v as u32;
    }
    if let Some(v) = a.get("from_op").and_then(|v| v.as_bool()) {
        an.from_op = v;
    }
    Ok(())
}

fn interp(x: &[f64], y: &[f64], t: f64) -> f64 {
    let k = x.partition_point(|v| *v < t);
    if k == 0 {
        return y.first().copied().unwrap_or(0.0);
    }
    if k >= x.len() {
        return y.last().copied().unwrap_or(0.0);
    }
    let f = (t - x[k - 1]) / (x[k] - x[k - 1]).max(1e-300);
    y[k - 1] + f * (y[k] - y[k - 1])
}

fn interp_log(x: &[f64], y: &[f64], f: f64) -> f64 {
    let lx: Vec<f64> = x.iter().map(|v| v.ln()).collect();
    interp(&lx, y, f.max(1e-30).ln())
}

pub fn tools() -> Vec<Value> {
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req});
    let pcb = json!({"type": "integer", "minimum": 0, "description": "PCB tab index (from create_pcb)"});
    let time = |d: &str| json!({"type": ["number", "string"], "description": d});
    let strs = |d: &str| json!({"type": "array", "items": {"type": "string"}, "description": d});
    let source = json!({"type": "object", "properties": {
        "name": {"type": "string", "description": "source name, for I(name) probes"},
        "plus": {"type": "string", "description": "net on the + terminal"},
        "minus": {"type": "string", "description": "net on the − terminal (default ground)"},
        "wave": {"type": ["number", "string"], "description": "5, DC 5, PULSE(v1 v2 delay rise fall width period), SIN(offset amplitude freq delay damping) or PWL(t1 v1 t2 v2 …); SI prefixes (1m, 10u, 1k; M is mega)"},
        "ac": {"type": "number", "description": "AC sweep magnitude (give the input 1)"},
        "current": {"type": "boolean", "description": "a current source (out of + into the circuit) instead of a voltage source"},
    }, "required": ["plus"], "additionalProperties": false});
    let analysis = json!({
        "stop": time("transient stop time, s (default five of the longest RC time constants or source periods)"),
        "step": time("longest transient step, s (default stop/500)"),
        "from_op": {"type": "boolean", "description": "start the transient from the DC operating point instead of from rest with the supplies switched on at 0"},
        "f_start": time("AC sweep start, Hz (default 10)"),
        "f_stop": time("AC sweep stop, Hz (default 1 MHz)"),
        "per_decade": {"type": "integer", "description": "AC points per decade (default 20)"},
    });
    let mut setup = analysis.clone();
    setup["pcb"] = pcb.clone();
    setup["sources"] = json!({"type": "array", "items": source, "description": "replace the sources (an empty list goes back to a DC supply on every power net, by name: VCC 5 V, +3V3 3.3 V, +12V 12 V)"});
    setup["ground"] = json!({"type": "string", "description": "ground net (default the ground ports or GND)"});
    setup["closed"] = strs("switches that are pressed (SW1 …); the others are open");
    setup["probes"] = strs("signals to plot and report: net names or V(NET), V(A,B), I(R1), I(U1.OUT), I(source); a Pico's GPIOs are also V(U1.GP0) … V(U1.GP29)");
    setup["mcus"] = json!({"type": "array", "description": "firmware for Raspberry Pi Pico parts (symbol PICO): it runs during transients, reading the circuit's voltages on its GPIOs and ADC pins and driving its pins", "items": {"type": "object", "properties": {
        "part": {"type": "string", "description": "the Pico's reference (U1)"},
        "firmware": {"type": "string", "description": "path of a .uf2, .elf, .hex or .bin built for the RP2040 (Pico SDK, Arduino-Pico); empty for none"},
        "usb": {"type": "boolean", "description": "powered from USB: 5 V on VBUS (default true)"},
        "uart_in": {"type": "string", "description": "text arriving on UART0 RX (GP1) at the start"},
    }, "required": ["part"], "additionalProperties": false}});
    let mut sim = analysis;
    sim["pcb"] = pcb;
    sim["analysis"] = json!({"type": "string", "enum": ["op", "tran", "ac"], "description": "op: DC operating point; tran: transient; ac: small-signal sweep about the operating point (default the last one run, else tran)"});
    sim["probes"] = strs("signals to report and plot (default the setup's, else the net voltages)");
    sim["at"] =
        json!({"type": "array", "items": {"type": "number"}, "description": "also give each probe's value at these times (tran) or gain and phase at these frequencies (ac)"});
    sim["plot"] = json!({"type": "boolean", "description": "return the waveform plot as an image"});
    sim["path"] = json!({"type": "string", "description": "save the plot PNG here (and return it)"});
    sim["width"] = json!({"type": "integer", "description": "plot width px (default 1000)"});
    sim["csv"] = json!({"type": "string", "description": "write the probed waveforms as CSV here"});
    vec![
        json!({"name": "pcb_sim_setup", "description": "Set up the circuit simulation of a PCB tab's schematic: sources (by default a DC supply on every power net, named like VCC, +5V, 3V3), ground net, pressed switches, probes and analysis settings. Returns each part's model (by symbol and value: resistors 4k7, capacitors 100n, inductors, 1N4148/1N4007/1N5819/BAT54/zener diodes, LEDs by colour, BC847/2N3904/2N2222/BC857/2N3906 BJTs, 2N7002/BSS138/AO3400/AO3401/IRLZ44N MOSFETs, NE555/TLC555 timers, LM358/LM324/TL072/NE5532/MCP6002/LMV321 op amps, 78xx/LM317/AMS1117/AP2112/MCP1700 regulators, Raspberry Pi Pico boards running firmware; connectors, crystals and other ICs are left open), the signal names for probes, and notes.", "inputSchema": obj(setup, &["pcb"])}),
        json!({"name": "simulate_pcb", "description": "Simulate a PCB tab's schematic (SPICE-like: Newton-Raphson DC, variable-step BDF2 transient, AC about the operating point; Pico boards run their firmware during transients, so transients default to at least 1 s with them). op returns net voltages, source currents and each part's pin voltages, currents and power. tran returns per probe min, max, average, RMS, final value and, when it repeats, period, frequency and duty cycle (from the second cycle on). ac returns gain at the start, peak and the −3 dB frequencies. Picos report their serial (UART) output with times, notes and pin states. The plot (plot: true or path) shows the probes; csv writes the waveforms.", "inputSchema": obj(sim, &["pcb"])}),
    ]
}
