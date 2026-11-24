//! The standard two-station part job (`plan_part_job`): a part (or a pair of parts on one blank) machined
//! from both sides in a two-station vise, op 1 in one station from the raw blank and op 2 in the other from
//! what op 1 left, as one rCAD job. From the measured stock, its material, the vise stations and the part,
//! it writes the setups and the standard operations the DMC2 jobs were made of, following its CAM rules:
//!
//! - op 1: Find stock (from the saved vise location), face the blank's top (the part's bottom), adaptive
//!   roughing from the power budget in laps round the parts down to just over the jaws, the whole side
//!   profile finished in one full-height climb pass with arc leads, the carrier's locating bores, edge break;
//! - op 2 (flipped, on op 1's finished face): Find stock, a stop to blow the carrier bores out, Find bores,
//!   face down to the part's top, a light rough of the band above op 1's walls (it separates the parts),
//!   the top chamfer that hides the op 1 / op 2 seam, edge break.
//!
//! Every operation has a key; `ops` overrides any of its arguments, `skip` leaves it out and `extra` puts the
//! part's own operations (pockets, channels, 3D surfaces, engraving) before or after one. All operations are
//! added with `build: false`, so the whole plan builds once at the end. The result lists the exact tool calls,
//! so the plan can be kept as a script and run again.

use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Speeds and feeds for one material (the DMC2's, from its jobs).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct Speeds {
    pub rough_rpm: f64,
    pub face_rpm: f64,
    pub fin_rpm: f64,
    pub ch_rpm: f64,
    pub feed: f64,
    pub fin_feed: f64,
    pub plunge: f64,
    pub ch_feed: f64,
    pub bore_feed: f64,
    /// Roughing power target, W (the cutting model's, at the shaft).
    pub target: f64,
    /// Op 2's top facing: stepover, mm.
    pub face2_stepover: f64,
}

impl Speeds {
    /// 6061: rough at 9k to the power budget, face and finish at 12k, the chamfer mill at 24k (light).
    pub fn aluminium() -> Speeds {
        Speeds { rough_rpm: 9000.0, face_rpm: 12000.0, fin_rpm: 12000.0, ch_rpm: 24000.0, feed: 3000.0, fin_feed: 2000.0, plunge: 400.0, ch_feed: 1850.0, bore_feed: 1500.0, target: 650.0, face2_stepover: 0.2 }
    }

    /// C360: no measurements yet and the high-helix cutter pulls in: mild at 18k, the budget well under its caution.
    pub fn brass() -> Speeds {
        Speeds { rough_rpm: 18000.0, face_rpm: 18000.0, fin_rpm: 18000.0, ch_rpm: 18000.0, feed: 1350.0, fin_feed: 1000.0, plunge: 250.0, ch_feed: 1000.0, bore_feed: 1000.0, target: 300.0, face2_stepover: 0.4 }
    }

    pub fn for_material(m: &str) -> Speeds {
        let m = m.to_ascii_lowercase();
        if m.contains("brass") || m.contains("c360") || m.contains("bronze") {
            Speeds::brass()
        } else {
            Speeds::aluminium()
        }
    }
}

impl Default for Speeds {
    fn default() -> Self {
        Speeds::aluminium()
    }
}

/// The geometry the plan needs from the document and the machine.
pub struct Facts {
    /// The setups already in the document (the new ones get the next indexes).
    pub cams: usize,
    /// The parts' height (thickness), mm.
    pub part_t: f64,
    /// Op 1's and op 2's stations: the lowest the tool goes beside the jaws above the ledges (grip + clearance).
    pub jaw_limit: [f64; 2],
    /// The power the spindle may take at a speed before it's a caution, W.
    pub caution: Box<dyn Fn(f64) -> f64>,
    /// The machine settings (`machine` of create_cam) for both setups; null: the document's.
    pub machine: Value,
}

fn num(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(|v| v.as_f64())
}

fn r4(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

fn g(v: f64) -> String {
    crate::cam::fmt_mm(r4(v))
}

/// One planned operation: its key and the cam_op arguments.
struct Planned {
    key: String,
    args: Value,
}

/// The plan: every tool call in order, the numbers it worked from, and warnings.
pub struct Plan {
    pub steps: Vec<Value>,
    pub summary: Value,
    pub warnings: Vec<String>,
}

/// Plan the job from the call's arguments `a` (see the tool's description) and the facts.
pub fn plan(a: &Value, f: &Facts) -> Result<Plan, String> {
    let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("Part").trim().to_string();
    let studio = a.get("studio").and_then(|v| v.as_u64()).ok_or("plan_part_job: give studio (the Part Studio of the part)")?;
    let stock = a.get("stock").ok_or("plan_part_job: give stock {size [x, y, z] as measured, material}")?;
    let size: [f64; 3] = stock.get("size").and_then(|v| serde_json::from_value(v.clone()).ok()).ok_or("plan_part_job: stock.size [x, y, z], the blank as measured")?;
    let material = stock.get("material").and_then(|v| v.as_str()).ok_or("plan_part_job: stock.material (e.g. 6061-T6, Brass C360)")?.to_string();
    let mut sp = Speeds::for_material(&material);
    if let Some(s) = a.get("speeds") {
        let mut base = serde_json::to_value(SpeedsOut(&sp)).unwrap_or_default();
        for (k, v) in s.as_object().ok_or("speeds: an object")? {
            base[k] = v.clone();
        }
        sp = serde_json::from_value(base).map_err(|e| format!("speeds: {e}"))?;
    }
    let (w, l, h) = (size[0], size[1], size[2]);
    let face = num(a, "face").unwrap_or(0.2);
    let t = f.part_t;
    let op1 = a.get("op1").cloned().unwrap_or(json!({}));
    let op2 = a.get("op2").cloned().unwrap_or(json!({}));
    let shim = num(&op1, "shim").unwrap_or(0.0);
    let shim2 = num(&op2, "shim").unwrap_or(0.0);
    if h - face < t - 1e-9 {
        return Err(format!("the blank ({} thick, faced {}) is thinner than the part ({})", g(h), g(face), g(t)));
    }
    // op 1 in blank coordinates: Z0 the blank's bottom (on the ledges or its shims)
    let top1 = h - face;
    let low1 = f.jaw_limit[0] - shim;
    // op 2: how high op 1's walls reach above the part's bottom
    let wall_top = top1 - low1;
    let cap = |rpm: f64| (f.caution)(rpm).round();
    let mut warnings = vec![];
    let seam = num(a, "seam_chamfer");
    match seam {
        Some(c) if t - c > wall_top - 0.2 + 1e-9 => warnings.push(format!(
            "the seam chamfer {} reaches down to Z{} in op 2, but op 1's walls only reach Z{}: the op 1 / op 2 seam is below the chamfer (it needs a lower edge 0.2 under the walls' top: chamfer {} or more, or thinner shims / a taller blank)",
            g(c),
            g(t - c),
            g(wall_top),
            g(t - wall_top + 0.2)
        )),
        None if wall_top < t - 1e-9 => warnings.push(format!(
            "op 1's walls reach Z{} of the part's {}: op 2 cuts the rest of the wall, and the seam shows; give seam_chamfer (at least {}) to hide it",
            g(wall_top),
            g(t),
            g(t - wall_top + 0.2)
        )),
        _ => {}
    }
    if low1 >= top1 {
        return Err(format!("op 1 can't cut beside the jaws: the blank top (Z{}) is not above the jaw limit (Z{})", g(top1), g(low1)));
    }
    let carrier = a.get("carrier").cloned().unwrap_or(json!({}));
    let bores: Vec<[f64; 2]> = match carrier.get("points") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| "carrier.points: [[x, y], …]")?,
        None => vec![],
    };
    let bore_d = num(&carrier, "diameter").unwrap_or(7.5);
    let bore_probe_depth = num(&carrier, "probe_depth").unwrap_or(1.0);
    if bores.len() == 1 {
        return Err("carrier.points: two bores (Find bores needs two), or none".into());
    }
    if bores.is_empty() {
        warnings.push("no carrier bores: op 2 is located by Find stock alone (the rules want Find stock then Find bores or a notch)".into());
    }
    let levels1 = num(a, "rough_levels").map(|v| v.max(1.0).round()).unwrap_or(((top1 - low1) / 6.0).ceil().max(1.0));
    let face2 = top1 - t;
    // op 2's facing in levels of up to 2 mm
    let levels2 = num(a, "face2_levels").map(|v| v.max(1.0).round()).unwrap_or((face2 / 2.0 - 1e-9).ceil().max(1.0));
    let (c1, c2) = (f.cams, f.cams + 1);
    let n1 = format!("{name} op1 bottom");
    let n2 = format!("{name} op2 top");

    // ---- tools (the chamfer mill first: cam_op's tool takes ids before numbers, so T1 is id 1) ----
    let tl = a.get("tools").cloned().unwrap_or(json!({}));
    let tool = |key: &str, base: Value| -> Value {
        let mut v = base;
        if let Some(o) = tl.get(key).and_then(|o| o.as_object()) {
            for (k, x) in o {
                v[k] = x.clone();
            }
        }
        v
    };
    let chamfer = tool("chamfer", json!({"preset": "ALUMINUM", "name": "ALUMINUM chamfer 90", "number": 1, "holder": "ER16 collet chuck", "stickout": 22, "rpm": sp.ch_rpm, "feed": sp.ch_feed, "plunge_feed": 200}));
    let flat = tool("flat", json!({"preset": "3 FLUTE DLC 1/4", "name": "3 FLUTE DLC 1/4", "number": 24, "holder": "ER16 collet chuck", "stickout": 24, "rpm": sp.rough_rpm, "feed": sp.feed, "plunge_feed": sp.plunge, "stepdown": 3, "stepover": 0.15}));
    let probe = tool("probe", json!({"preset": "Touch probe", "name": "YIDOFENG 3D touch probe", "number": 99, "diameter": 2.0, "flute_length": 12, "holder": "ER16 collet chuck"}));
    let tnum = |v: &Value| v.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
    let (tc, tf, tp) = (tnum(&chamfer), tnum(&flat), tnum(&probe));
    let tools_for = |c: usize| -> Vec<Value> {
        [&chamfer, &flat, &probe]
            .iter()
            .map(|t| {
                let mut a = (*t).clone();
                a["cam"] = json!(c);
                json!({"tool": "cam_tool", "args": a})
            })
            .collect()
    };

    // ---- the setups ----
    let common = |c: &mut Map<String, Value>| {
        for (k, v) in [("post", json!("rcnc")), ("max_rpm", json!(24000)), ("rapid_rate", json!(5000)), ("clearance", json!(5)), ("retract", json!(2)), ("stay_down", json!(true))] {
            c.insert(k.into(), v);
        }
        if !f.machine.is_null() {
            c.insert("machine".into(), f.machine.clone());
        }
    };
    let zref1 = op1.get("z_reference").and_then(|v| v.as_str()).map(String::from).unwrap_or(if shim > 0.0 { String::new() } else { "Vise".into() });
    let mut s1 = Map::new();
    common(&mut s1);
    let mut vise1 = json!({"station": op1.get("station").and_then(|v| v.as_str()).unwrap_or("left")});
    if shim > 0.0 {
        vise1["shim"] = json!(shim);
        vise1["shims"] = op1.get("shims").cloned().ok_or("op1.shim needs op1.shims (the strips' outlines, work X/Y)")?;
    }
    let mut vise2 = json!({"station": op2.get("station").and_then(|v| v.as_str()).unwrap_or("right")});
    if shim2 > 0.0 {
        vise2["shim"] = json!(shim2);
        vise2["shims"] = op2.get("shims").cloned().ok_or("op2.shim needs op2.shims")?;
    }
    for (k, v) in [
        ("name", json!(n1)),
        ("studio", json!(studio)),
        ("orient", op1.get("orient").cloned().unwrap_or(json!("bottom"))),
        ("origin", json!("stock_bottom_center")),
        ("stock", json!({"size": [w, l, h], "bottom": r4(h - face - t), "material": material})),
        ("work_offset", json!(op1.get("work_offset").and_then(|v| v.as_u64()).unwrap_or(1))),
        ("with", json!(std::iter::once(n2.clone()).chain(a.get("with").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str().map(String::from))).collect::<Vec<_>>())),
        ("probe", json!({"z_reference": zref1})),
        ("vise", vise1),
        ("rcnc", json!({"require_guard": a.get("require_guard").and_then(|v| v.as_bool()).unwrap_or(true)})),
    ] {
        s1.insert(k.into(), v);
    }
    if let Some(b) = a.get("bodies") {
        s1.insert("bodies".into(), b.clone());
    }
    let mut s2 = Map::new();
    common(&mut s2);
    for (k, v) in [
        ("name", json!(n2)),
        ("studio", json!(studio)),
        ("follows", json!(n1)),
        ("origin", op2.get("origin").cloned().unwrap_or(json!("part_bottom_center"))),
        ("work_offset", json!(op2.get("work_offset").and_then(|v| v.as_u64()).unwrap_or(2))),
        ("probe", json!({"z_reference": op2.get("z_reference").and_then(|v| v.as_str()).unwrap_or("Vise")})),
        ("vise", vise2),
        ("rcnc", json!({"require_guard": a.get("require_guard").and_then(|v| v.as_bool()).unwrap_or(true)})),
    ] {
        s2.insert(k.into(), v);
    }
    if let Some(b) = a.get("bodies") {
        s2.insert("bodies".into(), b.clone());
    }

    // ---- op 1 ----
    let op = |c: usize, key: &str, kind: &str, name: String, rest: Value| -> Planned {
        let mut v = rest;
        v["cam"] = json!(c);
        v["kind"] = json!(kind);
        v["name"] = json!(name);
        Planned { key: key.into(), args: v }
    };
    let rough = |rpm: f64, target: f64, cap_w: f64, rest: Value| -> Value {
        let mut v = json!({"tool": tf, "leave": 0.3, "rpm": rpm, "feed": sp.feed, "chip_thinning": true, "power_target": target, "power_cap": cap_w, "feed_schedule": true});
        for (k, x) in rest.as_object().cloned().unwrap_or_default() {
            v[k] = x;
        }
        v
    };
    let mut ops1 = vec![
        op(c1, "probe_top", "probe", "Probe stock top".into(), json!({"tool": tp, "probing": {"cycle": "z", "depth": 3, "tolerance": 1.0}})),
        op(c1, "probe_centre", "probe", "Probe stock centre".into(), json!({"tool": tp, "probing": {"cycle": "boss", "depth": 3, "tolerance": 1.0}})),
        op(c1, "face", "face", format!("Face the bar top (the parts' bottoms) {}", g(face)), json!({"tool": tf, "stepdown": face, "stepover": 0.4, "rpm": sp.face_rpm, "feed": sp.feed})),
        op(
            c1,
            "rough",
            "adaptive",
            format!("Rough round the parts, the gap and the ends to Z{} (laps, power budget)", g(low1)),
            rough(sp.rough_rpm, sp.target, num(a, "rough_cap").unwrap_or(cap(sp.rough_rpm)), json!({"bottom": r4(low1), "stepdown": r4((top1 - low1) / levels1), "leave_floor": 0.0})),
        ),
        op(
            c1,
            "walls",
            "contour",
            format!("Finish the walls in one full-height pass, down to Z{}", g(low1)),
            json!({"tool": tf, "side": "outside", "bottom": r4(low1), "stepdown": 20, "lead": 2, "rpm": sp.fin_rpm, "feed": sp.fin_feed}),
        ),
    ];
    if !bores.is_empty() {
        ops1.push(op(
            c1,
            "carrier_bores",
            "bore",
            format!("Two locating bores {} through the carrier (op 2 finds them)", g(bore_d)),
            json!({"tool": tf, "points": bores, "thread_diameter": bore_d, "bottom": -0.3, "rpm": sp.rough_rpm, "feed": sp.bore_feed}),
        ));
    }
    ops1.push(op(c1, "edge_break", "chamfer", "Edge break 0.3 the bottom outline".into(), json!({"tool": tc, "chamfer": 0.3, "side": "outside", "tip_offset": 1.0})));

    // ---- op 2 ----
    let mut ops2 = vec![
        op(c2, "probe_top", "probe", "Probe stock top".into(), json!({"tool": tp, "probing": {"cycle": "z", "depth": 3, "tolerance": 1.5}})),
        op(c2, "probe_centre", "probe", "Probe stock centre".into(), json!({"tool": tp, "probing": {"cycle": "boss", "depth": 3, "tolerance": 1.5}})),
    ];
    for (i, xy) in bores.iter().enumerate() {
        let mut v = json!({"tool": tp, "at": xy, "probing": {"cycle": "bore", "on_part": true, "size": bore_d, "depth": bore_probe_depth, "tolerance": 0.1, "max_turn": 0.5}});
        if i == 0 {
            v["pause"] = json!("Blow the chips out of the two carrier bores between the parts, and check for burrs at their mouths");
        }
        ops2.push(op(c2, if i == 0 { "bore_a" } else { "bore_b" }, "probe", format!("Probe carrier bore {}", ["A", "B"][i.min(1)]), v));
    }
    ops2.push(op(
        c2,
        "face",
        "face",
        format!("Face the top down to Z{} ({}, {} level{})", g(t), g(face2), levels2, if levels2 > 1.0 { "s" } else { "" }),
        json!({"tool": tf, "stepdown": r4(face2 / levels2), "stepover": sp.face2_stepover, "rpm": sp.face_rpm, "feed": sp.feed}),
    ));
    if wall_top - 0.2 < t {
        ops2.push(op(
            c2,
            "band",
            "adaptive",
            format!("Rough the band above op 1's walls off to Z{}, 0.2 under their top Z{} (no web left), 0.3 off them (separates the parts)", g(wall_top - 0.2), g(wall_top)),
            rough(sp.rough_rpm, 120.0, 150.0, json!({"bottom": r4(wall_top - 0.2), "stepdown": r4(t - wall_top + 0.2), "leave_floor": 0.0})),
        ));
    }
    if let Some(c) = seam {
        ops2.push(op(c2, "seam_chamfer", "chamfer", format!("Chamfer {} x 45 the top outline (takes the 0.3 left on the band and hides the op 1 / op 2 seam)", g(c)), json!({"tool": tc, "chamfer": c, "side": "outside", "tip_offset": 1.0})));
    }
    ops2.push(op(c2, "edge_break", "chamfer", "Edge break 0.3 the top edges inside".into(), json!({"tool": tc, "chamfer": 0.3, "side": "inside", "tip_offset": 0.7})));

    // ---- overrides, skips and the part's own operations ----
    let skip: Vec<String> = match a.get("skip") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| "skip: [\"op1.face\", \"op2.edge_break\", …]")?,
        None => vec![],
    };
    let ov = a.get("ops").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut lists = [ops1, ops2];
    for (k, v) in &ov {
        let (s, key) = k.split_once('.').ok_or_else(|| format!("ops: keys are op1.<key> or op2.<key>, not {k}"))?;
        let i = match s {
            "op1" => 0,
            "op2" => 1,
            _ => return Err(format!("ops: {k}: op1 or op2")),
        };
        let keys = lists[i].iter().map(|p| format!("{s}.{}", p.key)).collect::<Vec<_>>().join(", ");
        let p = lists[i].iter_mut().find(|p| p.key == key).ok_or_else(|| format!("ops: no {k} (keys: {keys})"))?;
        for (kk, x) in v.as_object().ok_or_else(|| format!("ops: {k}: an object of cam_op arguments"))? {
            if x.is_null() {
                if let Some(o) = p.args.as_object_mut() {
                    o.remove(kk);
                }
            } else {
                p.args[kk] = x.clone();
            }
        }
    }
    for k in &skip {
        let (s, key) = k.split_once('.').ok_or_else(|| format!("skip: op1.<key> or op2.<key>, not {k}"))?;
        let i = if s == "op1" { 0 } else if s == "op2" { 1 } else { return Err(format!("skip: {k}")) };
        let n = lists[i].len();
        lists[i].retain(|p| p.key != key);
        if lists[i].len() == n {
            return Err(format!("skip: no {k}"));
        }
    }
    if let Some(ex) = a.get("extra") {
        for (j, e) in ex.as_array().ok_or("extra: a list of {setup, before | after, op}")?.iter().enumerate() {
            let s = e.get("setup").and_then(|v| v.as_str()).unwrap_or("op2");
            let i = if s == "op1" { 0 } else if s == "op2" { 1 } else { return Err(format!("extra {j}: setup op1 or op2")) };
            let mut args = e.get("op").cloned().ok_or_else(|| format!("extra {j}: op (cam_op arguments, with kind)"))?;
            if args.get("kind").is_none() {
                return Err(format!("extra {j}: op needs kind"));
            }
            args["cam"] = json!([c1, c2][i]);
            let key = e.get("key").and_then(|v| v.as_str()).map(String::from).unwrap_or(format!("extra{j}"));
            let at = match (e.get("before").and_then(|v| v.as_str()), e.get("after").and_then(|v| v.as_str())) {
                (Some(b), _) => lists[i].iter().position(|p| p.key == b).ok_or_else(|| format!("extra {j}: no {s}.{b}"))?,
                (None, Some(af)) => lists[i].iter().position(|p| p.key == af).ok_or_else(|| format!("extra {j}: no {s}.{af}"))? + 1,
                // default: before the edge break, which goes last
                (None, None) => lists[i].iter().position(|p| p.key == "edge_break").unwrap_or(lists[i].len()),
            };
            lists[i].insert(at, Planned { key, args });
        }
    }

    let mut steps = vec![json!({"tool": "create_cam", "args": Value::Object(s1)})];
    steps.extend(tools_for(c1));
    for p in &lists[0] {
        let mut a = p.args.clone();
        a["build"] = json!(false);
        steps.push(json!({"tool": "cam_op", "args": a}));
    }
    steps.push(json!({"tool": "create_cam", "args": Value::Object(s2)}));
    steps.extend(tools_for(c2));
    for p in &lists[1] {
        let mut a = p.args.clone();
        a["build"] = json!(false);
        steps.push(json!({"tool": "cam_op", "args": a}));
    }
    let summary = json!({
        "setups": [{"cam": c1, "name": n1}, {"cam": c2, "name": n2}],
        "part_thickness": r4(t),
        "blank": [w, l, h],
        "material": material,
        "op1": {"top": r4(top1), "jaw_limit": r4(low1), "rough_levels": levels1, "z_reference": zref1, "shim": shim, "operations": lists[0].iter().map(|p| p.key.clone()).collect::<Vec<_>>()},
        "op2": {"walls_reach": r4(wall_top), "face": r4(face2), "face_levels": levels2, "operations": lists[1].iter().map(|p| p.key.clone()).collect::<Vec<_>>()},
        "power": {"rough_rpm": sp.rough_rpm, "target_w": sp.target, "cap_w": num(a, "rough_cap").unwrap_or(cap(sp.rough_rpm))},
    });
    Ok(Plan { steps, summary, warnings })
}

/// Speeds as JSON, for overlaying the call's `speeds` on them.
struct SpeedsOut<'a>(&'a Speeds);

impl serde::Serialize for SpeedsOut<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        json!({"rough_rpm": p.rough_rpm, "face_rpm": p.face_rpm, "fin_rpm": p.fin_rpm, "ch_rpm": p.ch_rpm, "feed": p.feed, "fin_feed": p.fin_feed, "plunge": p.plunge, "ch_feed": p.ch_feed, "bore_feed": p.bore_feed, "target": p.target, "face2_stepover": p.face2_stepover}).serialize(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(t: f64) -> Facts {
        // the DMC2: 1118 W at 9000, falling as (9000 / rpm)^0.76 above, caution 0.76
        let caution = |rpm: f64| 1118.0 * 0.76 * (rpm / 9000.0).max(1.0).powf(-0.76);
        Facts { cams: 0, part_t: t, jaw_limit: [5.0, 5.0], caution: Box::new(caution), machine: Value::Null }
    }

    fn base() -> Value {
        json!({"name": "Base", "studio": 3, "stock": {"size": [26.6, 101, 12.34], "material": "6061-T6"}, "carrier": {"points": [[-5, 0], [5, 0]]}, "seam_chamfer": 1.7})
    }

    fn ops(p: &Plan, cam: u64) -> Vec<&Value> {
        p.steps.iter().filter(|s| s["tool"] == "cam_op" && s["args"]["cam"] == cam).map(|s| &s["args"]).collect()
    }

    #[test]
    fn the_ingot_base() {
        let p = plan(&base(), &facts(8.6)).unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        let s1 = &p.steps[0]["args"];
        assert_eq!(s1["stock"]["bottom"], json!(3.54));
        assert_eq!(s1["with"], json!(["Base op2 top"]));
        assert_eq!(s1["vise"]["station"], "left");
        assert_eq!(s1["probe"]["z_reference"], "Vise");
        let o1 = ops(&p, 0);
        let k: Vec<&str> = o1.iter().map(|o| o["kind"].as_str().unwrap()).collect();
        assert_eq!(k, ["probe", "probe", "face", "adaptive", "contour", "bore", "chamfer"]);
        // the rough: two levels of 3.57 from the faced top 12.14 down to the jaw limit 5, 650 W capped at the caution
        let r = o1[3];
        assert_eq!((r["bottom"].as_f64(), r["stepdown"].as_f64(), r["power_target"].as_f64(), r["power_cap"].as_f64()), (Some(5.0), Some(3.57), Some(650.0), Some(850.0)));
        assert!(o1.iter().all(|o| o["build"] == false));
        let o2 = ops(&p, 1);
        let k: Vec<&str> = o2.iter().map(|o| o["name"].as_str().unwrap()).collect();
        assert!(k[2].starts_with("Probe carrier bore A") && o2[2]["pause"].as_str().unwrap().contains("bores"), "{k:?}");
        // op 2 faces 12.14 - 8.6 = 3.54 in two levels; the band to 0.2 under op 1's walls (7.14); the seam chamfer
        assert_eq!(o2[4]["stepdown"].as_f64(), Some(1.77));
        assert_eq!(o2[5]["bottom"].as_f64(), Some(6.94));
        assert_eq!(o2[6]["chamfer"].as_f64(), Some(1.7));
        assert_eq!(p.summary["op2"]["walls_reach"], json!(7.14));
    }

    #[test]
    fn seam_warnings_shims_and_extras() {
        // a seam chamfer too small for where op 1's walls end
        let mut a = base();
        a["seam_chamfer"] = json!(1.0);
        let p = plan(&a, &facts(8.6)).unwrap();
        assert!(p.warnings.iter().any(|w| w.contains("below the chamfer")), "{:?}", p.warnings);
        // brass on shims: Z from the probed top, the jaw limit lower by the shim, mild speeds
        let a = json!({"name": "Slider", "studio": 4, "stock": {"size": [25, 101, 10], "material": "Brass C360"}, "carrier": {"points": [[-5, 0], [5, 0]]}, "seam_chamfer": 2.4,
            "op1": {"shim": 1.67, "shims": [{"min": [-12.5, -45], "max": [12.5, -6]}, {"min": [-12.5, 6], "max": [12.5, 45]}]},
            "rough_levels": 4, "rough_cap": 400, "face2_levels": 1,
            "ops": {"op1.edge_break": {"side": "both"}, "op2.face": {"stepover": 0.4}},
            "skip": ["op2.edge_break"],
            "extra": [{"setup": "op1", "after": "walls", "key": "pockets", "op": {"kind": "bore", "tool": 24, "spring_pass": true}}]});
        let p = plan(&a, &facts(8.0)).unwrap();
        assert_eq!(p.steps[0]["args"]["probe"]["z_reference"], "");
        let o1 = ops(&p, 0);
        assert_eq!(o1[3]["bottom"].as_f64(), Some(3.33));
        assert_eq!(o1[3]["power_cap"].as_f64(), Some(400.0));
        assert_eq!(o1[3]["rpm"].as_f64(), Some(18000.0));
        assert_eq!(o1[5]["kind"], "bore");
        assert_eq!(o1[5]["spring_pass"], true);
        assert_eq!(o1.last().unwrap()["side"], "both");
        let o2 = ops(&p, 1);
        assert!(o2.iter().all(|o| o["name"] != "Edge break 0.3 the top edges inside"));
        assert_eq!(p.summary["op2"]["walls_reach"], json!(6.47));
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        // mistakes are named
        let mut b = base();
        b["skip"] = json!(["op2.nothing"]);
        assert!(plan(&b, &facts(8.6)).err().unwrap().contains("op2.nothing"));
        let mut b = base();
        b["stock"]["size"] = json!([26.6, 101, 8.0]);
        assert!(plan(&b, &facts(8.6)).err().unwrap().contains("thinner than the part"));
    }
}
