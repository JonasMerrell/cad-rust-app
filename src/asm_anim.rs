//! Assembly animations. A *motion* is a slider (0–100 %) linked to joints: each link moves one mate
//! (a slider's travel, a revolute's angle) from one value to another over part of the slider's range,
//! so one slider can move several joints at their own rates or one after another. A link can also
//! drive another motion, chaining them. An *animation* is a timeline of clips, each running one
//! motion from one position to another over a stretch of time; clips can overlap or follow on.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::assembly::{Assembly, Drive, MateKind};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ease {
    #[default]
    Linear,
    /// Starts and stops gently.
    Smooth,
}

impl Ease {
    pub const ALL: [Ease; 2] = [Ease::Linear, Ease::Smooth];
    pub fn label(self) -> &'static str {
        match self {
            Ease::Linear => "Linear",
            Ease::Smooth => "Smooth",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Ease::Linear => "linear",
            Ease::Smooth => "smooth",
        }
    }
    fn apply(self, f: f64) -> f64 {
        match self {
            Ease::Linear => f,
            Ease::Smooth => f * f * (3.0 - 2.0 * f),
        }
    }
}

/// What a link moves.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// A mate's value: mm along a slider (or a cylindrical's axis), degrees about a revolute.
    Mate(u64),
    /// A cylindrical mate's turn (degrees).
    Turn(u64),
    /// Another motion's position (0..1).
    Motion(u64),
}

impl Target {
    pub fn mate(self) -> Option<u64> {
        match self {
            Target::Mate(m) | Target::Turn(m) => Some(m),
            Target::Motion(_) => None,
        }
    }
}

fn one() -> f64 {
    1.0
}

/// One joint a motion moves: from `from` to `to` while the motion's slider goes from `start` to `end` (0..1).
/// Before `start` it waits at `from`, after `end` it stays at `to`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Link {
    pub target: Target,
    pub from: f64,
    pub to: f64,
    #[serde(default)]
    pub start: f64,
    #[serde(default = "one")]
    pub end: f64,
    #[serde(default)]
    pub ease: Ease,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Motion {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub links: Vec<Link>,
    /// Where its slider was left (0..1).
    #[serde(default)]
    pub value: f64,
}

/// A stretch of the timeline: motion `motion` runs from `from` to `to` (0..1) between `start` and `start + length` seconds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clip {
    pub motion: u64,
    pub start: f64,
    pub length: f64,
    #[serde(default)]
    pub from: f64,
    #[serde(default = "one")]
    pub to: f64,
    #[serde(default)]
    pub ease: Ease,
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.length.max(0.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Once,
    Loop,
    /// Forwards then backwards.
    Bounce,
}

impl Repeat {
    pub const ALL: [Repeat; 3] = [Repeat::Once, Repeat::Loop, Repeat::Bounce];
    pub fn label(self) -> &'static str {
        match self {
            Repeat::Once => "Play once",
            Repeat::Loop => "Loop",
            Repeat::Bounce => "Back and forth",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Repeat::Once => "once",
            Repeat::Loop => "loop",
            Repeat::Bounce => "bounce",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Animation {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub repeat: Repeat,
    /// Start playing when the web page opens.
    #[serde(default)]
    pub autoplay: bool,
}

impl Animation {
    pub fn duration(&self) -> f64 {
        self.clips.iter().map(|c| c.end()).fold(0.0, f64::max)
    }
}

/// A piece of a piecewise motion: `from` → `to` while x goes `a` → `b`.
#[derive(Clone, Copy, Debug)]
pub struct Seg {
    pub a: f64,
    pub b: f64,
    pub from: f64,
    pub to: f64,
    pub ease: Ease,
}

/// The value at `x` of segments run in order of their starts; and the start of the segment in charge
/// (None before the first, where it waits at the first's `from`).
pub fn piecewise(segs: &[Seg], x: f64) -> Option<(f64, Option<f64>)> {
    let mut s: Vec<&Seg> = segs.iter().collect();
    s.sort_by(|p, q| p.a.total_cmp(&q.a));
    let first = *s.first()?;
    let Some(cur) = s.iter().rev().find(|g| g.a <= x + 1e-12) else { return Some((first.from, None)) };
    let span = cur.b - cur.a;
    let f = if span <= 1e-12 { 1.0 } else { ((x - cur.a) / span).clamp(0.0, 1.0) };
    Some((cur.from + (cur.to - cur.from) * cur.ease.apply(f), Some(cur.a)))
}

fn link_seg(l: &Link) -> Seg {
    Seg { a: l.start.min(l.end), b: l.start.max(l.end), from: l.from, to: l.to, ease: l.ease }
}

/// Where each motion is: its own slider (or `own`), or what a motion linked to it puts it at. `rank`
/// orders the motions (the latest touched slider, or the latest started clip; missing is lowest): a
/// motion drives another only when it ranks at least as high, and the driven one then takes its rank.
pub fn motion_values(asm: &Assembly, own: &HashMap<u64, f64>, rank: &mut HashMap<u64, f64>) -> HashMap<u64, f64> {
    let mut v: HashMap<u64, f64> = asm.motions.iter().map(|m| (m.id, own.get(&m.id).copied().unwrap_or(m.value))).collect();
    let r = |rank: &HashMap<u64, f64>, id: u64| rank.get(&id).copied().unwrap_or(f64::NEG_INFINITY);
    // follow chains of motions (a loop just stops after as many passes as there are motions)
    for _ in 0..asm.motions.len() {
        let mut changed = false;
        for m in &asm.motions {
            let x = v[&m.id];
            let mut by: HashMap<u64, Vec<Seg>> = HashMap::new();
            for l in &m.links {
                if let Target::Motion(t) = l.target {
                    if t != m.id && v.contains_key(&t) {
                        by.entry(t).or_default().push(link_seg(l));
                    }
                }
            }
            for (t, segs) in by {
                let rm = r(rank, m.id);
                if rm < r(rank, t) {
                    continue;
                }
                if let Some((y, _)) = piecewise(&segs, x) {
                    let y = y.clamp(0.0, 1.0);
                    if (v[&t] - y).abs() > 1e-12 || r(rank, t) != rm {
                        v.insert(t, y);
                        if rm > f64::NEG_INFINITY {
                            rank.insert(t, rm);
                        }
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    v
}

/// Motions that another motion drives (their own slider does nothing).
pub fn driven_motions(asm: &Assembly) -> Vec<u64> {
    let ids: Vec<u64> = asm.motions.iter().map(|m| m.id).collect();
    asm.motions
        .iter()
        .flat_map(|m| m.links.iter().filter_map(move |l| if let Target::Motion(t) = l.target { (t != m.id).then_some(t) } else { None }))
        .filter(|t| ids.contains(t))
        .collect()
}

/// The mate values the motions put the joints at. A joint several motions move goes with the one
/// ranked highest in `rank` (see motion_values), else the last in the list.
pub fn mate_values(asm: &Assembly, values: &HashMap<u64, f64>, rank: &HashMap<u64, f64>) -> Vec<(Target, f64)> {
    let mut best: HashMap<Target, ((f64, usize), f64)> = HashMap::new();
    for (k, m) in asm.motions.iter().enumerate() {
        let x = values.get(&m.id).copied().unwrap_or(m.value);
        let mut by: Vec<(Target, Vec<Seg>)> = vec![];
        for l in m.links.iter().filter(|l| l.target.mate().is_some()) {
            match by.iter_mut().find(|(t, _)| *t == l.target) {
                Some((_, s)) => s.push(link_seg(l)),
                None => by.push((l.target, vec![link_seg(l)])),
            }
        }
        let key = (rank.get(&m.id).copied().unwrap_or(f64::NEG_INFINITY), k);
        for (t, segs) in by {
            let Some((y, _)) = piecewise(&segs, x) else { continue };
            if best.get(&t).map(|(b, _)| b.0 < key.0 || (b.0 == key.0 && b.1 < key.1)).unwrap_or(true) {
                best.insert(t, (key, y));
            }
        }
    }
    let mut out: Vec<(Target, f64)> = best.into_iter().map(|(t, (_, y))| (t, y)).collect();
    out.sort_by_key(|(t, _)| (t.mate(), matches!(t, Target::Turn(_))));
    out
}

/// The solver drives for those mate values (mates that are gone or can't move are left out).
pub fn drives(asm: &Assembly, vals: &[(Target, f64)]) -> Vec<Drive> {
    let mut out: Vec<Drive> = vec![];
    for &(t, v) in vals {
        let Some(id) = t.mate() else { continue };
        let Some(m) = asm.mates.iter().find(|m| m.id == id && !m.suppressed) else { continue };
        let (angle, slide) = match (m.kind, t) {
            (MateKind::Revolute, _) => (Some(v), None),
            (MateKind::Cylindrical, Target::Turn(_)) => (Some(v), None),
            (MateKind::Slider | MateKind::Cylindrical, _) => (None, Some(v)),
            _ => continue,
        };
        match out.iter_mut().find(|d| d.mate == id) {
            Some(d) => {
                d.angle = d.angle.or(angle);
                d.slide = d.slide.or(slide);
            }
            None => out.push(Drive { mate: id, angle, slide }),
        }
    }
    out
}

/// Where an animation's motions are at time `t` (s, within the animation) and since when each has been
/// running (its rank: the latest started clip wins a joint). Motions without clips keep their sliders.
pub fn animation_values(asm: &Assembly, anim: &Animation, t: f64) -> (HashMap<u64, f64>, HashMap<u64, f64>) {
    let mut own = HashMap::new();
    let mut rank = HashMap::new();
    for m in &asm.motions {
        let segs: Vec<Seg> = anim.clips.iter().filter(|c| c.motion == m.id).map(|c| Seg { a: c.start, b: c.end(), from: c.from, to: c.to, ease: c.ease }).collect();
        if let Some((v, since)) = piecewise(&segs, t) {
            own.insert(m.id, v.clamp(0.0, 1.0));
            rank.insert(m.id, since.unwrap_or(f64::NEG_INFINITY));
        }
    }
    let vals = motion_values(asm, &own, &mut rank);
    (vals, rank)
}

/// Time within the animation for a play head that has run `t` seconds (repeats folded in).
pub fn fold_time(anim: &Animation, t: f64) -> f64 {
    let d = anim.duration();
    if d <= 1e-9 {
        return 0.0;
    }
    match anim.repeat {
        Repeat::Once => t.clamp(0.0, d),
        Repeat::Loop => t.rem_euclid(d),
        Repeat::Bounce => {
            let u = t.rem_euclid(2.0 * d);
            if u > d { 2.0 * d - u } else { u }
        }
    }
}

/// The mates a motion can move: (id, name, kind).
pub fn movable_mates(asm: &Assembly) -> Vec<(u64, String, MateKind)> {
    asm.mates.iter().filter(|m| matches!(m.kind, MateKind::Slider | MateKind::Revolute | MateKind::Cylindrical) && !m.suppressed).map(|m| (m.id, m.name.clone(), m.kind)).collect()
}

/// A mate's current value (mm or degrees) as a link would set it.
pub fn current_value(asm: &Assembly, t: Target) -> Option<f64> {
    let id = t.mate()?;
    let m = asm.mates.iter().find(|m| m.id == id)?;
    let (a, b) = (asm.world(m.a.as_ref()?)?, asm.world(m.b.as_ref()?)?);
    Some(match (m.kind, t) {
        (MateKind::Revolute, _) | (MateKind::Cylindrical, Target::Turn(_)) => asm.mate_turn(m, a, b).to_degrees(),
        _ => crate::assembly::mate_slide(m, a, b),
    })
}

/// A new link for target `t`: over the mate's limits, or from where it is now a little way on.
pub fn new_link(asm: &Assembly, t: Target) -> Link {
    let (from, to) = match t {
        Target::Motion(_) => (0.0, 1.0),
        _ => {
            let id = t.mate().unwrap();
            let m = asm.mates.iter().find(|m| m.id == id);
            let turn = m.map(|m| m.kind == MateKind::Revolute || matches!(t, Target::Turn(_))).unwrap_or(false);
            match m.and_then(|m| m.limits).filter(|_| !matches!(t, Target::Turn(_))) {
                Some([lo, hi]) => (lo, hi),
                None => {
                    let v = current_value(asm, t).unwrap_or(0.0);
                    (v, v + if turn { 360.0 } else { 50.0 })
                }
            }
        }
    };
    Link { target: t, from, to, start: 0.0, end: 1.0, ease: Ease::Linear }
}

/// Unit of a target's values.
pub fn unit(asm: &Assembly, t: Target) -> &'static str {
    match t {
        Target::Motion(_) => "%",
        Target::Turn(_) => "°",
        Target::Mate(id) => match asm.mates.iter().find(|m| m.id == id).map(|m| m.kind) {
            Some(MateKind::Revolute) => "°",
            _ => "mm",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::Mate;

    fn mate(id: u64, kind: MateKind) -> Mate {
        Mate { id, name: format!("m{id}"), kind, a: None, b: None, flip: false, offset: 0.0, offset_xy: [0.0; 2], rotation: 0.0, limits: None, suppressed: false }
    }

    fn link(target: Target, from: f64, to: f64, start: f64, end: f64) -> Link {
        Link { target, from, to, start, end, ease: Ease::Linear }
    }

    fn asm() -> Assembly {
        let mut a = Assembly { name: "a".into(), ..Default::default() };
        a.mates = vec![mate(1, MateKind::Slider), mate(2, MateKind::Revolute), mate(3, MateKind::Cylindrical)];
        a
    }

    fn val(v: &[(Target, f64)], t: Target) -> f64 {
        v.iter().find(|x| x.0 == t).unwrap().1
    }

    #[test]
    fn one_slider_moves_joints_at_ratios_and_in_sequence() {
        let mut a = asm();
        a.motions = vec![Motion {
            id: 10,
            name: "Go".into(),
            value: 0.25,
            links: vec![link(Target::Mate(1), 0.0, 100.0, 0.0, 0.5), link(Target::Mate(1), 100.0, 0.0, 0.5, 1.0), link(Target::Mate(2), 0.0, 90.0, 0.0, 1.0)],
        }];
        let v = motion_values(&a, &HashMap::new(), &mut HashMap::new());
        let m = mate_values(&a, &v, &HashMap::new());
        assert!((val(&m, Target::Mate(1)) - 50.0).abs() < 1e-9);
        assert!((val(&m, Target::Mate(2)) - 22.5).abs() < 1e-9);
        let v = motion_values(&a, &HashMap::from([(10, 0.75)]), &mut HashMap::new());
        let m = mate_values(&a, &v, &HashMap::new());
        assert!((val(&m, Target::Mate(1)) - 50.0).abs() < 1e-9, "second half comes back");
        let v = motion_values(&a, &HashMap::from([(10, 1.0)]), &mut HashMap::new());
        assert!(val(&mate_values(&a, &v, &HashMap::new()), Target::Mate(1)).abs() < 1e-9);
    }

    #[test]
    fn a_motion_drives_another() {
        let mut a = asm();
        a.motions = vec![
            Motion { id: 10, name: "Master".into(), value: 0.5, links: vec![link(Target::Motion(11), 0.0, 1.0, 0.0, 1.0)] },
            Motion { id: 11, name: "Slave".into(), value: 0.0, links: vec![link(Target::Mate(1), 0.0, 10.0, 0.0, 1.0)] },
        ];
        let v = motion_values(&a, &HashMap::new(), &mut HashMap::new());
        assert!((v[&11] - 0.5).abs() < 1e-9);
        assert!((val(&mate_values(&a, &v, &HashMap::new()), Target::Mate(1)) - 5.0).abs() < 1e-9);
        assert_eq!(driven_motions(&a), vec![11]);
        // a loop doesn't hang
        a.motions[1].links.push(link(Target::Motion(10), 1.0, 0.0, 0.0, 1.0));
        let _ = motion_values(&a, &HashMap::new(), &mut HashMap::new());
    }

    #[test]
    fn timeline_sequences_and_latest_clip_wins() {
        let mut a = asm();
        a.motions = vec![
            Motion { id: 10, name: "Out".into(), value: 0.0, links: vec![link(Target::Mate(1), 0.0, 100.0, 0.0, 1.0)] },
            Motion { id: 11, name: "Back".into(), value: 0.0, links: vec![link(Target::Mate(1), 100.0, 20.0, 0.0, 1.0), link(Target::Turn(3), 0.0, 720.0, 0.0, 1.0)] },
        ];
        let anim = Animation {
            id: 20,
            name: "Cycle".into(),
            repeat: Repeat::Loop,
            autoplay: false,
            clips: vec![
                Clip { motion: 10, start: 0.0, length: 2.0, from: 0.0, to: 1.0, ease: Ease::Linear },
                Clip { motion: 11, start: 3.0, length: 1.0, from: 0.0, to: 1.0, ease: Ease::Linear },
            ],
        };
        assert_eq!(anim.duration(), 4.0);
        let at = |t: f64| {
            let (v, r) = animation_values(&a, &anim, t);
            mate_values(&a, &v, &r)
        };
        assert!((val(&at(1.0), Target::Mate(1)) - 50.0).abs() < 1e-9);
        assert!((val(&at(2.5), Target::Mate(1)) - 100.0).abs() < 1e-9, "holds between clips");
        assert!((val(&at(3.5), Target::Mate(1)) - 60.0).abs() < 1e-9, "the later clip takes the joint");
        assert!((val(&at(3.5), Target::Turn(3)) - 360.0).abs() < 1e-9);
        let d = drives(&a, &at(3.5));
        let c = d.iter().find(|d| d.mate == 3).unwrap();
        assert_eq!(c.angle, Some(360.0));
        assert!(c.slide.is_none());
        assert!((fold_time(&anim, 5.0) - 1.0).abs() < 1e-9);
        let b = Animation { repeat: Repeat::Bounce, ..anim.clone() };
        assert!((fold_time(&b, 5.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_clip_beats_an_idle_driver_and_the_latest_touch_wins() {
        let mut a = asm();
        a.motions = vec![
            Motion { id: 10, name: "X".into(), value: 0.0, links: vec![link(Target::Mate(1), 0.0, 100.0, 0.0, 1.0)] },
            Motion { id: 11, name: "Square".into(), value: 0.5, links: vec![link(Target::Mate(1), 20.0, 80.0, 0.0, 1.0)] },
            Motion { id: 12, name: "Home".into(), value: 0.0, links: vec![link(Target::Motion(10), 1.0, 0.0, 0.0, 1.0)] },
        ];
        let anim = Animation {
            id: 20,
            name: "A".into(),
            repeat: Repeat::Once,
            autoplay: false,
            clips: vec![Clip { motion: 10, start: 0.0, length: 1.0, from: 0.0, to: 1.0, ease: Ease::Linear }],
        };
        let (v, r) = animation_values(&a, &anim, 0.25);
        assert!((v[&10] - 0.25).abs() < 1e-9, "Home (no clip) leaves X's clip alone");
        assert!((val(&mate_values(&a, &v, &r), Target::Mate(1)) - 25.0).abs() < 1e-9);
        // sliders: Square touched after X, then Home touched last drives X, which takes the joint back
        let mut rank = HashMap::from([(10, 1.0), (11, 2.0)]);
        let v = motion_values(&a, &HashMap::new(), &mut rank);
        assert!((val(&mate_values(&a, &v, &rank), Target::Mate(1)) - 50.0).abs() < 1e-9);
        let mut rank = HashMap::from([(10, 1.0), (11, 2.0), (12, 3.0)]);
        let own = HashMap::from([(12, 1.0)]);
        let v = motion_values(&a, &own, &mut rank);
        assert_eq!(rank[&10], 3.0);
        assert!(val(&mate_values(&a, &v, &rank), Target::Mate(1)).abs() < 1e-9);
    }

    #[test]
    fn saved_without_animations_has_no_new_keys() {
        let a = asm();
        let s = serde_json::to_string(&a).unwrap();
        assert!(!s.contains("motions") && !s.contains("animations"));
        let mut a = a;
        a.motions.push(Motion { id: 4, name: "M".into(), value: 0.0, links: vec![link(Target::Turn(3), 0.0, 1.0, 0.0, 1.0)] });
        let back: Assembly = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back.motions, a.motions);
    }
}
