//! Vise stations: a machine's vise described once on its model (`cam_machine` `vise`), and a setup that says
//! which station holds its work (`create_cam` `vise`). From that rCAD works out what a job script used to:
//! the jaws and shims as fixtures, the lowest the tool may go beside the jaws, where the work sits on the
//! table (`on_table`), Find stock from the saved vise location (rcnc `VISE=`), and the machine model with
//! that station's moving jaw closed on the work for the simulation.

use std::sync::{Arc, Mutex, OnceLock};

use glam::{DVec3, Vec3};
use serde::{Deserialize, Serialize};

use crate::cam::{Cam, Fixture};
use crate::cam_machine::{Loaded, MachineModel};
use crate::cam_paths::{self, Part};

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// A part of the machine model that moves with a vise station's moving jaw.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MovingPart {
    /// The part's name (its path in the machine's STEP assembly), or an assembly that holds it.
    pub part: String,
    /// It moves only in whole steps of this, mm (a jaw base bolted into the fixture plate's hole grid); 0: freely.
    #[serde(skip_serializing_if = "is_zero")]
    pub grid: f64,
}

/// One station of a vise on the machine's table, in the machine model's coordinates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ViseStation {
    pub name: String,
    /// X of the fixed jaw's face.
    pub fixed_face: f64,
    /// The side of the work the fixed jaw is on: 1 = its +X side (the work lies on the jaw's -X side), -1 = -X.
    pub fixed_side: f64,
    /// Y of the jaws' middle.
    pub y: f64,
    /// Z of the parallels (ledges) the work, or its shims, sits on.
    pub ledge_z: f64,
    /// How high the jaws grip above the ledges, mm.
    pub grip: f64,
    pub jaw_length: f64,
    /// How thick the jaws are drawn as fixtures, mm (from their faces outward).
    pub jaw_thickness: f64,
    /// How far over the jaws' top the tool stays beside them, mm (the setup's fixture clearance).
    pub clearance: f64,
    /// What the fixed jaw is called on the setup sheet and in the program.
    pub fixed_name: String,
    /// The moving jaw's parts, moved along X to close on the work, and where its face is as modelled.
    pub moving: Vec<MovingPart>,
    pub moving_face: f64,
}

impl Default for ViseStation {
    fn default() -> Self {
        ViseStation {
            name: String::new(),
            fixed_face: 0.0,
            fixed_side: 1.0,
            y: 0.0,
            ledge_z: 0.0,
            grip: 4.7,
            jaw_length: 95.5,
            jaw_thickness: 12.7,
            clearance: 0.3,
            fixed_name: "fixed jaw".into(),
            moving: vec![],
            moving_face: 0.0,
        }
    }
}

impl ViseStation {
    pub fn side(&self) -> f64 {
        if self.fixed_side < 0.0 {
            -1.0
        } else {
            1.0
        }
    }

    /// Where the moving jaw's face goes to hold work `width` wide.
    pub fn moving_face_for(&self, width: f64) -> f64 {
        self.fixed_face - self.side() * width
    }
}

/// A shim strip under the work.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Shim {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its outline in work X, Y: min and max corners.
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// Which vise station holds a setup's work, and what it sits on.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ViseSetup {
    /// The machine model's station (`cam_machine` `vise`), e.g. "left".
    pub station: String,
    /// Width the jaws close on, mm. None: the stock box's X size for fresh stock; the part's for a setup that
    /// follows another (op 2 gripping the walls op 1 finished).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// Thickness of the shim strips under the work, mm (0: it sits on the ledges).
    #[serde(skip_serializing_if = "is_zero")]
    pub shim: f64,
    /// The shim strips (needed with `shim`): outlines in work X, Y.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shims: Vec<Shim>,
    /// Set by rCAD: the width the jaws were closed on, and the fixtures it made (replaced each time).
    #[serde(skip_serializing_if = "is_zero")]
    pub closed_width: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub made: Vec<String>,
}

/// What `apply` worked out, for the tool's result.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub width: f64,
    /// Work Z of the ledges, of the jaws' top, and the lowest the tool goes beside them.
    pub ledge: f64,
    pub jaw_top: f64,
    pub lowest_beside: f64,
    /// How much of the work's height the jaws hold.
    pub held: f64,
    pub notes: Vec<String>,
}

/// Put setup `c`'s work in its vise station of machine `m`: the jaw and shim fixtures (replacing the ones it
/// made before), the fixture clearance, `on_table`, and the rcnc vise side. `part` is the setup's part.
pub fn apply(c: &mut Cam, part: &Part, m: &MachineModel) -> Result<Applied, String> {
    let Some(mut v) = c.vise.clone() else { return Ok(Applied::default()) };
    let st = m.vise.iter().find(|s| s.name.eq_ignore_ascii_case(v.station.trim())).ok_or_else(|| {
        if m.vise.is_empty() {
            format!("vise: machine {} has no vise stations (define them with cam_machine vise)", m.name)
        } else {
            format!("vise: machine {} has no station {:?}; it has {}", m.name, v.station, m.vise.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "))
        }
    })?;
    if v.shim < 0.0 {
        return Err("vise: shim can't be below 0".into());
    }
    if v.shim > 0.0 && v.shims.is_empty() {
        return Err("vise: shim needs shims (their outlines, work X/Y min and max)".into());
    }
    c.fixtures.retain(|f| !v.made.contains(&f.name));
    let origin = cam_paths::work_origin(c, part);
    let (lo, hi) = cam_paths::stock_box(c, part);
    let follows = !c.follows.is_empty();
    // what the jaws close on, and what sits on the ledges (or the shims)
    let (glo, ghi) = if follows { (part.lo, part.hi) } else { (lo, hi) };
    let width = v.width.unwrap_or(ghi.x - glo.x);
    if !(width > 0.0) {
        return Err(format!("vise: the width to grip must be above 0 (is {width})"));
    }
    let gc = (glo.x + ghi.x) / 2.0;
    let rest = if follows { part.lo.z } else { lo.z };
    let ledge = rest - v.shim;
    let s = st.side();
    let ymid = (lo.y + hi.y) / 2.0;
    let (y0, y1) = (ymid - st.jaw_length / 2.0, ymid + st.jaw_length / 2.0);
    let (z0, z1) = (ledge, ledge + st.grip);
    let fixed_face = gc + s * width / 2.0;
    let moving_face = gc - s * width / 2.0;
    let side_name = |d: f64| if d > 0.0 { "+X" } else { "-X" };
    let w = |p: DVec3| (p - origin).to_array();
    let mut made = vec![];
    let mut boxes = vec![];
    let fixed = (DVec3::new(fixed_face.min(fixed_face + s * st.jaw_thickness), y0, z0), DVec3::new(fixed_face.max(fixed_face + s * st.jaw_thickness), y1, z1));
    let moving = (DVec3::new(moving_face.min(moving_face - s * st.jaw_thickness), y0, z0), DVec3::new(moving_face.max(moving_face - s * st.jaw_thickness), y1, z1));
    // listed -X first, as the setup sheet reads left to right
    let named = [(format!("{} ({})", st.fixed_name, side_name(s)), fixed), (format!("moving jaw ({})", side_name(-s)), moving)];
    let order: [usize; 2] = if s > 0.0 { [1, 0] } else { [0, 1] };
    for i in order {
        boxes.push((named[i].0.clone(), named[i].1));
    }
    for (i, sh) in v.shims.iter().enumerate() {
        let (a, b) = (DVec3::new(sh.min[0], sh.min[1], 0.0) + origin, DVec3::new(sh.max[0], sh.max[1], 0.0) + origin);
        let name = if sh.name.trim().is_empty() { format!("{} mm shim strip {}", crate::cam::fmt_mm(v.shim), i + 1) } else { sh.name.trim().to_string() };
        boxes.push((name, (DVec3::new(a.x.min(b.x), a.y.min(b.y), ledge), DVec3::new(a.x.max(b.x), a.y.max(b.y), rest))));
    }
    for (name, (a, b)) in boxes {
        // a fixture the user made under the same name gives way
        c.fixtures.retain(|f| f.name != name);
        c.fixtures.push(Fixture { name: name.clone(), min: w(a), max: w(b) });
        made.push(name);
    }
    c.fixture_clearance = st.clearance;
    // the setup's reference point (middle of the stock, at the bottom of whatever stands lowest) on the table
    let fx = cam_paths::fixture_boxes(c, part, origin);
    let r = crate::cam_machine::Placed::reference((lo, hi), &fx);
    let at = DVec3::new(st.fixed_face - s * width / 2.0 + (r.x - gc), st.y + (r.y - ymid), st.ledge_z + (r.z - ledge));
    c.on_table = (at - DVec3::from(m.table)).to_array().map(|x| (x * 1e6).round() / 1e6);
    c.rcnc.vise = if s > 0.0 { "left".into() } else { "right".into() };
    v.closed_width = width;
    v.made = made;
    c.vise = Some(v.clone());
    let top = hi.z;
    let mut out = Applied { width, ledge: ledge - origin.z, jaw_top: z1 - origin.z, lowest_beside: z1 + st.clearance - origin.z, held: (z1 - rest).min(top - rest).max(0.0), notes: vec![] };
    if v.shim > 0.0 && !c.probe.z_reference.trim().is_empty() {
        let zw = rest - origin.z;
        out.notes.push(format!(
            "the work sits on {} mm shims: its bottom is {} above the ledges, so a Z reference on the vise ({}) needs the shims in the work zero (take Z from the probed top instead: probe z_reference \"\")",
            crate::cam::fmt_mm(v.shim),
            crate::cam::fmt_mm(v.shim),
            if zw.abs() < 1e-6 { "Z0 is the work's bottom" } else { "check Z0" }
        ));
    }
    if out.held < 3.0 {
        out.notes.push(format!("the jaws hold only {} mm of the work: keep the cuts light", crate::cam::fmt_mm(out.held)));
    }
    Ok(out)
}

/// The stations' moving jaws closed: `closings` = (station, width) pairs. A station named more than once (a
/// job's op 2 and a later setup of the same part in the same station) closes once, on the first width.
pub fn closed(m: &MachineModel, closings: &[(String, f64)]) -> MachineModel {
    let mut m = m.clone();
    let mut done: Vec<String> = vec![];
    for (name, width) in closings {
        if done.iter().any(|d| d.eq_ignore_ascii_case(name)) {
            continue;
        }
        done.push(name.clone());
        let Some(st) = m.vise.iter().find(|s| s.name.eq_ignore_ascii_case(name)).cloned() else { continue };
        let dx = st.moving_face_for(*width) - st.moving_face;
        for mp in &st.moving {
            let d = if mp.grid > 0.0 { (dx / mp.grid).round() * mp.grid } else { dx };
            if d == 0.0 {
                continue;
            }
            for p in m.parts.iter_mut().filter(|p| p.name == mp.part || p.name.starts_with(&format!("{}/", mp.part))) {
                let tris: Vec<[Vec3; 3]> = crate::cam_machine::unpack_tris(&p.mesh).into_iter().map(|t| t.map(|q| q + Vec3::new(d as f32, 0.0, 0.0))).collect();
                p.mesh = crate::cam_machine::pack_tris(&tris);
            }
        }
    }
    m
}

/// Setup `c`'s machine model, with the vise jaws closed on its work (and, for a job, on every station's).
pub fn machine_for(c: &Cam) -> Result<Arc<Loaded>, String> {
    let l = crate::cam_machine::load(&c.machine.model)?;
    let closings = c.vise_closings();
    if closings.is_empty() || l.model.vise.is_empty() {
        return Ok(l);
    }
    let key = format!("{} @ {}", c.machine.model, closings.iter().map(|(n, w)| format!("{n}={w}")).collect::<Vec<_>>().join(","));
    static CACHE: OnceLock<Mutex<Vec<(String, Arc<Loaded>, Arc<Loaded>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(vec![]));
    if let Ok(v) = cache.lock() {
        // the same base machine (not edited since) closed the same way
        if let Some(x) = v.iter().find(|x| x.0 == key && Arc::ptr_eq(&x.1, &l)) {
            return Ok(x.2.clone());
        }
    }
    let closed = Arc::new(Loaded::new(closed(&l.model, &closings)));
    if let Ok(mut v) = cache.lock() {
        v.retain(|x| x.0 != key);
        v.push((key, l, closed.clone()));
        if v.len() > 16 {
            v.remove(0);
        }
    }
    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam_machine::MachinePart;

    fn vise_model() -> MachineModel {
        let jaw = crate::cam_machine::pack_tris(&crate::cam_machine::cuboid([12.7, 95.5, 20.0]));
        MachineModel {
            name: "Test".into(),
            parts: vec![
                MachinePart { name: "Vise/Base".into(), mesh: jaw.clone(), ..Default::default() },
                MachinePart { name: "Vise/Jaw".into(), mesh: jaw, ..Default::default() },
            ],
            table: [50.0, -350.0, 10.0],
            vise: vec![ViseStation {
                name: "left".into(),
                fixed_face: -7.8,
                fixed_side: 1.0,
                y: -350.0,
                ledge_z: 34.4,
                fixed_name: "fixed jaw (centre block)".into(),
                moving: vec![MovingPart { part: "Vise/Base".into(), grid: 25.0 }, MovingPart { part: "Vise/Jaw".into(), grid: 0.0 }],
                moving_face: -47.8,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn closing_moves_the_moving_jaw() {
        let m = vise_model();
        let c = closed(&m, &[("left".into(), 26.6)]);
        let x = |m: &MachineModel, i: usize| crate::cam_machine::unpack_tris(&m.parts[i].mesh).iter().flatten().map(|p| p.x).fold(f32::MAX, f32::min);
        // the face goes from -47.8 to -7.8 - 26.6 = -34.4: the jaw by 13.4, its base by the nearest whole 25
        assert!((x(&c, 1) - x(&m, 1) - 13.4).abs() < 1e-4);
        assert!((x(&c, 0) - x(&m, 0) - 25.0).abs() < 1e-4);
        // wider work: the face out to -57.8, 10 back, nearer no hole than this one
        let c = closed(&m, &[("left".into(), 50.0)]);
        assert!((x(&c, 1) - x(&m, 1) + 10.0).abs() < 1e-4);
        assert!((x(&c, 0) - x(&m, 0)).abs() < 1e-6);
        // unknown stations change nothing
        assert_eq!(closed(&m, &[("right".into(), 20.0)]), m);
        // a station a job names twice (op 2 and the engraving after it in the same station) closes once
        let once = closed(&m, &[("left".into(), 26.6)]);
        assert_eq!(closed(&m, &[("left".into(), 26.6), ("Left".into(), 26.6)]), once);
    }
}
