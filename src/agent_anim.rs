//! Agent tool for assembly animations: motion sliders linked to mates, timelines of them, posing and
//! rendering frames (see asm_anim).

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{R, Session};
use crate::asm_anim::{self, Animation, Clip, Ease, Link, Motion, Repeat, Target};
use crate::assembly::{Assembly, MateKind};

fn ease(v: Option<&Value>) -> R<Ease> {
    match v.and_then(|v| v.as_str()) {
        None => Ok(Ease::Linear),
        Some(s) => Ease::ALL.into_iter().find(|e| e.key() == s).ok_or(format!("ease: linear or smooth, not {s}")),
    }
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}

/// A motion by name or id.
fn motion_ref(asm: &Assembly, v: &Value) -> R<u64> {
    if let Some(id) = v.as_u64() {
        if asm.motions.iter().any(|m| m.id == id) {
            return Ok(id);
        }
    }
    let name = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
    asm.motions
        .iter()
        .find(|m| m.name == name || m.id.to_string() == name)
        .map(|m| m.id)
        .ok_or(format!("no motion {name}; there are {}", asm.motions.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")))
}

fn parse_link(asm: &Assembly, l: &Value, own: &str) -> R<Link> {
    let mate = |k: &str| -> R<Option<u64>> {
        let Some(id) = l.get(k).and_then(|v| v.as_u64()) else { return Ok(None) };
        let m = asm.mates.iter().find(|m| m.id == id).ok_or(format!("motion {own}: no mate {id}"))?;
        match (k, m.kind) {
            ("mate", MateKind::Slider | MateKind::Revolute | MateKind::Cylindrical) | ("turn", MateKind::Cylindrical) => Ok(Some(id)),
            ("turn", _) => Err(format!("motion {own}: turn is for cylindrical mates; give mate {id} for a {}", m.kind.label().to_lowercase())),
            _ => Err(format!("motion {own}: {} is a {} mate, which doesn't move", m.name, m.kind.label().to_lowercase())),
        }
    };
    let target = if let Some(id) = mate("mate")? {
        Target::Mate(id)
    } else if let Some(id) = mate("turn")? {
        Target::Turn(id)
    } else if let Some(m) = l.get("motion") {
        Target::Motion(motion_ref(asm, m)?)
    } else {
        return Err(format!("motion {own}: each link needs mate (id), turn (a cylindrical mate's id) or motion (name)"));
    };
    let mut link = asm_anim::new_link(asm, target);
    if let Some(v) = num(l, "from") {
        link.from = v;
    }
    if let Some(v) = num(l, "to") {
        link.to = v;
    }
    link.start = num(l, "start").unwrap_or(0.0).clamp(0.0, 1.0);
    link.end = num(l, "end").unwrap_or(1.0).clamp(0.0, 1.0);
    link.ease = ease(l.get("ease"))?;
    Ok(link)
}

fn motions_json(asm: &Assembly) -> Value {
    json!(
        asm.motions
            .iter()
            .map(|m| json!({
                "id": m.id,
                "name": m.name,
                "value": m.value,
                "links": m.links.iter().map(|l| {
                    let mut o = json!({"from": l.from, "to": l.to, "start": l.start, "end": l.end, "ease": l.ease.key(), "unit": asm_anim::unit(asm, l.target)});
                    match l.target {
                        Target::Mate(id) => o["mate"] = json!(id),
                        Target::Turn(id) => o["turn"] = json!(id),
                        Target::Motion(id) => o["motion"] = json!(asm.motions.iter().find(|x| x.id == id).map(|x| x.name.clone())),
                    }
                    o
                }).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>()
    )
}

fn animations_json(asm: &Assembly) -> Value {
    let name = |id: u64| asm.motions.iter().find(|m| m.id == id).map(|m| m.name.clone()).unwrap_or_default();
    json!(
        asm.animations
            .iter()
            .map(|a| json!({
                "id": a.id,
                "name": a.name,
                "repeat": a.repeat.key(),
                "autoplay": a.autoplay,
                "duration_s": a.duration(),
                "clips": a.clips.iter().map(|c| json!({"motion": name(c.motion), "start": c.start, "length": c.length, "from": c.from, "to": c.to, "ease": c.ease.key()})).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>()
    )
}

impl Session {
    fn anim_pose(&mut self, ai: usize, vals: &HashMap<u64, f64>, rank: &HashMap<u64, f64>) -> Vec<u64> {
        let a = &self.ws.assemblies[ai];
        let drives = asm_anim::drives(a, &asm_anim::mate_values(a, vals, rank));
        for m in self.ws.assemblies[ai].motions.iter_mut() {
            if let Some(v) = vals.get(&m.id) {
                m.value = *v;
            }
        }
        if drives.is_empty() { vec![] } else { self.solve_driven(ai, &drives) }
    }

    fn anim_pose_at(&mut self, ai: usize, an: &Animation, t: f64) -> Vec<u64> {
        let (v, r) = asm_anim::animation_values(&self.ws.assemblies[ai], an, asm_anim::fold_time(an, t));
        self.anim_pose(ai, &v, &r)
    }

    pub(super) fn anim_call(&mut self, a: &Value) -> R<(Value, Option<Vec<u8>>)> {
        let ai = a.get("assembly").and_then(|v| v.as_u64()).ok_or("give assembly (its index)")? as usize;
        if ai >= self.ws.assemblies.len() {
            return Err(format!("no assembly {ai}"));
        }
        let mut out = json!({});
        if let Some(list) = a.get("motions") {
            let list = list.as_array().ok_or("motions: a list of {name, links: [...]}")?;
            // names first (so links can drive motions defined later in the list), keeping ids of motions that stay
            let mut asm = self.ws.assemblies[ai].clone();
            let old = std::mem::take(&mut asm.motions);
            for m in list {
                let name = m.get("name").and_then(|v| v.as_str()).ok_or("each motion needs a name")?.to_string();
                let id = old.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| asm.new_id());
                asm.motions.push(Motion { id, name, links: vec![], value: num(m, "value").unwrap_or(0.0).clamp(0.0, 1.0) });
            }
            for (k, m) in list.iter().enumerate() {
                let own = asm.motions[k].name.clone();
                let links = m.get("links").and_then(|v| v.as_array()).ok_or(format!("motion {own}: give links"))?;
                let links: Vec<Link> = links.iter().map(|l| parse_link(&asm, l, &own)).collect::<R<_>>()?;
                asm.motions[k].links = links;
            }
            let ids: Vec<u64> = asm.motions.iter().map(|m| m.id).collect();
            for an in &mut asm.animations {
                an.clips.retain(|c| ids.contains(&c.motion));
            }
            self.ws.assemblies[ai] = asm;
        }
        if let Some(list) = a.get("animations") {
            let list = list.as_array().ok_or("animations: a list of {name, clips: [...]}")?;
            let mut asm = self.ws.assemblies[ai].clone();
            let old = std::mem::take(&mut asm.animations);
            for an in list {
                let name = an.get("name").and_then(|v| v.as_str()).ok_or("each animation needs a name")?.to_string();
                let repeat = match an.get("repeat").and_then(|v| v.as_str()) {
                    None => Repeat::Once,
                    Some(s) => Repeat::ALL.into_iter().find(|r| r.key() == s).ok_or(format!("animation {name}: repeat is once, loop or bounce"))?,
                };
                let mut clips: Vec<Clip> = vec![];
                // a clip without a start follows the one before it
                let mut end = 0.0f64;
                for c in an.get("clips").and_then(|v| v.as_array()).ok_or(format!("animation {name}: give clips"))? {
                    let motion = motion_ref(&asm, c.get("motion").ok_or(format!("animation {name}: each clip needs a motion"))?)?;
                    let start = num(c, "start").unwrap_or(end).max(0.0);
                    let length = num(c, "length").unwrap_or(2.0).max(0.0);
                    let from = num(c, "from").unwrap_or(0.0).clamp(0.0, 1.0);
                    let to = num(c, "to").unwrap_or(1.0).clamp(0.0, 1.0);
                    clips.push(Clip { motion, start, length, from, to, ease: ease(c.get("ease"))? });
                    end = start + length;
                }
                let id = old.iter().find(|o| o.name == name).map(|o| o.id).unwrap_or_else(|| asm.new_id());
                asm.animations.push(Animation { id, name, clips, repeat, autoplay: an.get("autoplay").and_then(|v| v.as_bool()).unwrap_or(false) });
            }
            self.ws.assemblies[ai] = asm;
        }
        let needs_pose = a.get("set").is_some() || a.get("at").is_some() || a.get("frames").is_some();
        if needs_pose {
            self.build_for_assembly(ai)?;
            self.sync();
        }
        if let Some(set) = a.get("set") {
            let set = set.as_object().ok_or("set: {motion name: 0..1}")?;
            let (mut own, mut rank) = (HashMap::new(), HashMap::new());
            for (k, v) in set {
                let id = motion_ref(&self.ws.assemblies[ai], &json!(k))?;
                own.insert(id, v.as_f64().ok_or(format!("set.{k}: a number 0..1"))?.clamp(0.0, 1.0));
                // the motions set take their joints, the last one given winning
                rank.insert(id, rank.len() as f64);
            }
            let vals = asm_anim::motion_values(&self.ws.assemblies[ai], &own, &mut rank);
            let failed = self.anim_pose(ai, &vals, &rank);
            if !failed.is_empty() {
                out["failed_mates"] = json!(failed);
            }
        }
        let pick = |s: &Session, v: Option<&Value>| -> R<Animation> {
            let asm = &s.ws.assemblies[ai];
            match v {
                None => asm.animations.first().cloned().ok_or("no animations yet".to_string()),
                Some(v) => asm.animations.iter().find(|x| Some(x.name.as_str()) == v.as_str() || Some(x.id) == v.as_u64()).cloned().ok_or(format!("no animation {v}")),
            }
        };
        if let Some(at) = a.get("at") {
            let an = pick(self, at.get("animation"))?;
            let t = num(at, "time").ok_or("at: {animation, time (s)}")?;
            let failed = self.anim_pose_at(ai, &an, t);
            if !failed.is_empty() {
                out["failed_mates"] = json!(failed);
            }
            out["posed_at"] = json!({"animation": an.name, "time": asm_anim::fold_time(&an, t)});
        }
        if let Some(fr) = a.get("frames").filter(|f| f.is_object()) {
            let an = pick(self, fr.get("animation"))?;
            let path = fr.get("path").and_then(|v| v.as_str()).ok_or("frames: give path (a file name prefix, e.g. frames/printer)")?.to_string();
            let d = an.duration();
            let times: Vec<f64> = match fr.get("times").and_then(|v| v.as_array()) {
                Some(ts) => ts.iter().filter_map(|x| x.as_f64()).collect(),
                None => {
                    let n = fr.get("count").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 2000) as usize;
                    (0..n).map(|k| d * k as f64 / (n.max(2) - 1) as f64).collect()
                }
            };
            // one camera for every frame: framing all the poses
            let (mut lo, mut hi) = (glam::DVec3::splat(f64::INFINITY), glam::DVec3::splat(f64::NEG_INFINITY));
            let mut failed: Vec<u64> = vec![];
            for t in &times {
                failed.extend(self.anim_pose_at(ai, &an, *t));
                let (l, h) = self.assembly_bounds(ai)?;
                (lo, hi) = (lo.min(l), hi.max(h));
            }
            let mut saved = vec![];
            for (i, t) in times.iter().enumerate() {
                self.anim_pose_at(ai, &an, *t);
                let png = self.render_assembly_in(ai, fr, lo.x.is_finite().then_some((lo, hi)))?;
                let file = format!("{path}-{i:04}.png");
                crate::agent::write_out(&file, &png)?;
                saved.push(json!({"t": (t * 1000.0).round() / 1000.0, "path": file}));
            }
            failed.sort();
            failed.dedup();
            if !failed.is_empty() {
                out["failed_mates"] = json!(failed);
            }
            out["frames"] = json!(saved);
        }
        let asm = &self.ws.assemblies[ai];
        out["motions"] = motions_json(asm);
        out["animations"] = animations_json(asm);
        out["movable_mates"] =
            json!(asm_anim::movable_mates(asm).iter().map(|(id, name, kind)| json!({"id": id, "name": name, "kind": kind.label().to_lowercase()})).collect::<Vec<_>>());
        Ok((out, None))
    }
}

pub fn tools() -> Vec<Value> {
    let n = |d: &str| json!({"type": "number", "description": d});
    let ease = json!({"type": "string", "enum": ["linear", "smooth"], "description": "linear, or smooth (eases in and out); default linear"});
    let link = json!({"type": "object", "description": "one joint (or motion) this slider moves", "properties": {
        "mate": {"type": "integer", "description": "a slider, revolute or cylindrical mate's id: its travel (mm) or angle (degrees)"},
        "turn": {"type": "integer", "description": "or a cylindrical mate's id, for its turn (degrees)"},
        "motion": {"type": ["string", "integer"], "description": "or another motion (name), which this one drives: from/to are its slider positions 0..1"},
        "from": n("the value at the start of the span (default the mate's lower limit, or where it is now)"),
        "to": n("the value at the end of the span (default the upper limit, or 50 mm / 360° on)"),
        "start": n("where on this slider (0..1) the joint starts moving (default 0)"),
        "end": n("where it has arrived (default 1): spans one after another move joints in sequence"),
        "ease": ease.clone(),
    }, "additionalProperties": false});
    let clip = json!({"type": "object", "description": "one motion running over a stretch of time", "properties": {
        "motion": {"type": ["string", "integer"], "description": "motion name"},
        "start": n("start time s (default: when the previous clip ends, so clips run in sequence)"),
        "length": n("duration s (default 2)"),
        "from": n("motion slider position at the start, 0..1 (default 0)"),
        "to": n("position at the end, 0..1 (default 1)"),
        "ease": ease,
    }, "required": ["motion"], "additionalProperties": false});
    let frames = json!({"type": "object", "description": "render the animation frame by frame to PNGs", "properties": {
        "animation": {"type": ["string", "integer"], "description": "animation name (default the first)"},
        "path": {"type": "string", "description": "file name prefix, e.g. frames/printer"},
        "count": {"type": "integer", "description": "frames spread evenly over it (default 30)"},
        "times": {"type": "array", "items": {"type": "number"}, "description": "or these times (s)"},
        "view": {"type": "string", "description": "as in render_assembly"},
        "direction": {"type": "array", "items": {"type": "number"}, "description": "as in render_assembly"},
        "width": {"type": "integer", "description": "frame width px (default 800)"},
        "height": {"type": "integer", "description": "frame height px (default 600)"},
    }, "required": ["path"], "additionalProperties": false});
    vec![json!({
        "name": "assembly_animation",
        "description": "Assembly animations (shown in the Assembly tab's Animation section and played in export_web pages). motions replaces the motion sliders: each is a slider 0..1 with links that move mates (a slider's travel in mm, a revolute's angle in degrees) from one value to another over part of its range, so one slider can move several joints at their own rates or one after another, and drive other motions. animations replaces the timelines: clips run motions over time (in sequence by default, or overlapping with explicit starts); when two motions move one joint, the clip started last wins. set poses the assembly at motion slider positions; at poses it at a time of an animation; frames renders PNGs through one. Returns the motions, animations (with durations) and the mates that can move.",
        "inputSchema": {"type": "object", "properties": {
            "assembly": {"type": "integer", "minimum": 0, "description": "assembly index"},
            "motions": {"type": "array", "description": "all motion sliders (kept ids by name)", "items": {"type": "object", "description": "a motion slider", "properties": {
                "name": {"type": "string", "description": "its name (clips and other motions refer to it by this)"},
                "value": n("its slider position 0..1 (default 0)"),
                "links": {"type": "array", "description": "what it moves", "items": link},
            }, "required": ["name", "links"], "additionalProperties": false}},
            "animations": {"type": "array", "description": "all animations", "items": {"type": "object", "description": "a timeline", "properties": {
                "name": {"type": "string", "description": "its name"},
                "repeat": {"type": "string", "enum": ["once", "loop", "bounce"], "description": "once (default), loop, or bounce (forwards then back)"},
                "autoplay": {"type": "boolean", "description": "start playing when the web page opens"},
                "clips": {"type": "array", "description": "motions over time", "items": clip},
            }, "required": ["name", "clips"], "additionalProperties": false}},
            "set": {"type": "object", "description": "pose at motion slider positions: {motion name: 0..1}", "additionalProperties": {"type": "number", "description": "slider position 0..1"}},
            "at": {"type": "object", "description": "pose at a time of an animation", "properties": {
                "animation": {"type": ["string", "integer"], "description": "animation name (default the first)"},
                "time": n("seconds from its start"),
            }, "required": ["time"], "additionalProperties": false},
            "frames": frames,
        }, "required": ["assembly"]},
    })]
}
