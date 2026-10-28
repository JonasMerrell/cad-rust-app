//! A Part Studio or an Assembly as an interactive web page (see web_export). In an assembly every instance is
//! a node: slider and cylindrical mates become sliding joints and revolute mates turning ones, within the
//! mates' limits; gear and rack relations make one joint follow another; the explode steps drive the page's
//! explode slider. Instances reached through fastened, planar or ball mates, or not reached from a fixed
//! instance at all, ride rigidly where they are.

use std::collections::HashMap;

use glam::DVec3;

use crate::assembly::{self, MateKind, Pose, RelationKind};
use crate::drawing_ui::DocCtx;
use crate::kernel::BodyMesh;
use crate::web_export::{Drive, Joint, JointKind, Mesh, Part, Scene};

fn mesh_of(b: &BodyMesh) -> Mesh {
    let mut edges = vec![];
    for e in &b.edges {
        for w in e.pts.windows(2) {
            edges.extend_from_slice(&w[0]);
            edges.extend_from_slice(&w[1]);
        }
    }
    Mesh { positions: b.positions.clone(), indices: b.indices.clone(), edges }
}

/// 3 × 4 row-major.
fn rows(p: &Pose) -> [f64; 12] {
    let m = p.m();
    let mut out = [0.0; 12];
    for r in 0..3 {
        let row = m.row(r);
        out[4 * r..4 * r + 4].copy_from_slice(&[row.x, row.y, row.z, p.t[r]]);
    }
    out
}

fn inverse(p: &Pose) -> Pose {
    let rt = p.m().transpose();
    Pose { r: rt.to_cols_array(), t: (-(rt * DVec3::from(p.t))).into() }
}

/// Meshes shared by (studio, body), and the parts drawn with them.
struct Parts<'a> {
    ctx: &'a dyn DocCtx,
    meshes: Vec<Mesh>,
    index: HashMap<(usize, crate::doc::BodyId), usize>,
}

impl Parts<'_> {
    fn part(&mut self, studio: usize, body: crate::doc::BodyId, node: usize, local: Option<Pose>) -> Option<Part> {
        let b = self.ctx.bodies(studio).iter().find(|b| b.id == body)?;
        let props = self.ctx.studio_doc(studio).and_then(|d| d.parts.get(&crate::doc::body_key(body))).cloned().unwrap_or_default();
        if props.hidden {
            return None;
        }
        let mesh = match self.index.get(&(studio, body)) {
            Some(&k) => k,
            None => {
                self.meshes.push(mesh_of(b));
                self.index.insert((studio, body), self.meshes.len() - 1);
                self.meshes.len() - 1
            }
        };
        let color = props.color.map(|c| c.map(|v| v as f32 / 255.0)).unwrap_or([0.70, 0.72, 0.76]);
        let mut info = vec![("Volume".to_string(), format!("{:.2} cm³", b.volume.abs() / 1000.0))];
        if let Some(m) = &props.material {
            info.push(("Material".into(), m.clone()));
        }
        if let Some(s) = self.ctx.ws().studios.get(studio) {
            info.push(("Part Studio".into(), s.name.clone()));
        }
        Some(Part {
            name: props.name.unwrap_or_else(|| b.name.clone()),
            node,
            mesh,
            local: local.map(|p| rows(&p)),
            color,
            opacity: 1.0,
            shine: 0.3,
            edges: true,
            layer: props.material.clone(),
            info,
        })
    }
}

/// The parts of one Part Studio, fixed.
pub fn studio_scene(ctx: &dyn DocCtx, studio: usize) -> (Scene, Vec<Mesh>) {
    let name = ctx.ws().studios.get(studio).map(|s| s.name.clone()).unwrap_or_else(|| "Part Studio".into());
    let mut scene = Scene { title: name.clone(), ..Default::default() };
    let node = scene.node(name, None);
    let mut parts = Parts { ctx, meshes: vec![], index: HashMap::new() };
    let ids: Vec<_> = ctx.bodies(studio).iter().map(|b| b.id).collect();
    for id in ids {
        if let Some(p) = parts.part(studio, id, node, None) {
            scene.parts.push(p);
        }
    }
    (scene, parts.meshes)
}

/// An assembly with its moving joints. Also returns notes on mates the page can't show moving.
pub fn assembly_scene(ctx: &dyn DocCtx, ai: usize) -> Result<(Scene, Vec<Mesh>, Vec<String>), String> {
    let all = &ctx.ws().assemblies;
    let asm = all.get(ai).ok_or_else(|| format!("no assembly {ai}"))?;
    let insts: Vec<_> = asm.instances.iter().filter(|x| !x.hidden).collect();
    let node_of: HashMap<u64, usize> = insts.iter().enumerate().map(|(k, x)| (x.id, k)).collect();
    let mut notes = vec![];

    // a spanning tree from the fixed instances along the mates: (parent instance, the mate, child is its b side)
    let mut parent: HashMap<u64, Option<(u64, usize, bool)>> = HashMap::new();
    for x in insts.iter().filter(|x| x.fixed) {
        parent.insert(x.id, None);
    }
    if parent.is_empty() {
        if let Some(x) = insts.first() {
            parent.insert(x.id, None);
        }
    }
    let mates: Vec<(usize, &assembly::Mate)> = asm.mates.iter().enumerate().filter(|(_, m)| !m.suppressed && m.a.is_some() && m.b.is_some()).collect();
    let mut used = vec![false; asm.mates.len()];
    loop {
        let mut grew = false;
        for &(k, m) in &mates {
            let (a, b) = (m.a.unwrap().inst, m.b.unwrap().inst);
            if !node_of.contains_key(&a) || !node_of.contains_key(&b) {
                continue;
            }
            let (pa, pb) = (parent.contains_key(&a), parent.contains_key(&b));
            if pa && !pb {
                parent.insert(b, Some((a, k, true)));
            } else if pb && !pa {
                parent.insert(a, Some((b, k, false)));
            } else {
                continue;
            }
            used[k] = true;
            grew = true;
        }
        if !grew {
            break;
        }
    }
    for &(k, m) in &mates {
        if !used[k] && matches!(m.kind, MateKind::Slider | MateKind::Revolute | MateKind::Cylindrical) {
            notes.push(format!("{}: closes a loop, so it doesn't move on the page", m.name));
        }
    }

    let mut scene = Scene { title: asm.name.clone(), ..Default::default() };
    for x in &insts {
        scene.node(x.name.clone(), None);
    }
    let mut joint_of_mate: HashMap<u64, (usize, f64)> = HashMap::new();
    for x in &insts {
        let i = node_of[&x.id];
        let link = parent.get(&x.id).copied().flatten();
        let Some((p, k, child_is_b)) = link else {
            scene.nodes[i].base = Some(rows(&x.pose));
            continue;
        };
        let pp = asm.instance(p).unwrap().pose;
        let ip = inverse(&pp);
        scene.nodes[i].parent = Some(node_of[&p]);
        scene.nodes[i].base = Some(rows(&ip.compose(&x.pose)));
        let m = &asm.mates[k];
        let kind = match m.kind {
            MateKind::Slider | MateKind::Cylindrical => JointKind::Slider,
            MateKind::Revolute => JointKind::Revolute,
            MateKind::Fastened => continue,
            MateKind::Planar | MateKind::Ball => {
                notes.push(format!("{}: {} mates are shown fixed", m.name, m.kind.label().to_lowercase()));
                continue;
            }
        };
        let (fa, fb) = (asm.world(m.a.as_ref().unwrap()).unwrap(), asm.world(m.b.as_ref().unwrap()).unwrap());
        let (o, z, _) = assembly::target_frame(m, fa);
        let value = match kind {
            JointKind::Slider => assembly::mate_slide(m, fa, fb),
            JointKind::Revolute => assembly::mate_angle(m, fa, fb).to_degrees(),
        };
        // the child moving along +z raises the value when it is the mate's second part
        let axis = ip.m() * (if child_is_b { z } else { -z });
        let origin = ip.apply(o);
        scene.nodes[i].joint = Some(Joint {
            kind,
            origin: origin.into(),
            axis: axis.into(),
            limits: m.limits,
            home: value,
            value: None,
            label: Some(m.name.clone()),
            drive: None,
        });
        joint_of_mate.insert(m.id, (i, value));
    }
    // relations: one joint follows another
    for r in &asm.relations {
        let sign = if r.reverse { -1.0 } else { 1.0 };
        let (Some(&(na, va)), Some(&(nb, vb))) = (joint_of_mate.get(&r.a), r.b.and_then(|b| joint_of_mate.get(&b))) else {
            if r.kind != RelationKind::Screw {
                notes.push(format!("{}: one of its mates doesn't move on the page", r.name));
            }
            continue;
        };
        let ratio = match r.kind {
            RelationKind::Gear => -sign * r.ratio,
            RelationKind::Rack => sign * r.ratio * std::f64::consts::PI / 180.0,
            RelationKind::Screw => continue,
        };
        if let Some(j) = scene.nodes[nb].joint.as_mut() {
            j.drive = Some(Drive { of: na, ratio, offset: vb - ratio * va });
        }
    }
    // explode steps, in each node's parent frame
    for st in &asm.explode_steps {
        for id in &st.insts {
            let Some(&i) = node_of.get(id) else { continue };
            let d = DVec3::from(st.dir).normalize_or_zero() * st.distance;
            let d = match scene.nodes[i].parent {
                Some(p) => inverse(&insts[p].pose).dir(d),
                None => d,
            };
            let e = scene.nodes[i].explode.get_or_insert([0.0; 3]);
            for k in 0..3 {
                e[k] += d[k];
            }
        }
    }
    let mut parts = Parts { ctx, meshes: vec![], index: HashMap::new() };
    for x in &insts {
        let i = node_of[&x.id];
        for l in assembly::leaves(all, x) {
            let local = if x.sub.is_some() { Some(l.pose) } else { None };
            if let Some(mut p) = parts.part(l.studio, l.body, i, local) {
                if x.sub.is_none() {
                    p.name = x.name.clone();
                }
                scene.parts.push(p);
            }
        }
    }
    Ok((scene, parts.meshes, notes))
}
