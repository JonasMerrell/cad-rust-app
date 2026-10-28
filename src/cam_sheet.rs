//! Setup sheet: a printable HTML page for the operator with the stock, work zero, fixtures, tools and
//! operations of a setup, and a picture of the part on the machine.

use std::fmt::Write;

use base64::Engine;
use glam::DVec3;

use crate::cam::{fmt_mm, Cam, OpKind};
use crate::cam_paths::{self, Move, Part, Toolpath};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn time(s: f64) -> String {
    let s = s.max(0.0).round() as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn xyz(v: DVec3) -> String {
    format!("{}, {}, {}", fmt_mm(v.x), fmt_mm(v.y), fmt_mm(v.z))
}

/// The part as it sits on the machine (spindle up), with the stock, the fixtures and the work zero, as a PNG.
pub fn picture(cam: &Cam, part: &Part, w: u32, h: u32) -> Option<Vec<u8>> {
    let mut m = crate::kernel::BodyMesh::default();
    for t in &part.tris {
        for p in t {
            let i = m.positions.len() as u32 / 3;
            m.positions.extend([p.x as f32, p.y as f32, p.z as f32]);
            m.normals.extend([0.0, 0.0, 1.0]);
            m.indices.push(i);
        }
        m.tri_face.push(0);
    }
    m.bbox = [part.lo.to_array(), part.hi.to_array()];
    let boxes = |lo: DVec3, hi: DVec3, out: &mut Vec<Vec<DVec3>>| {
        let c = |k: usize| DVec3::new([lo.x, hi.x][k & 1], [lo.y, hi.y][(k >> 1) & 1], [lo.z, hi.z][(k >> 2) & 1]);
        out.push([0, 1, 3, 2, 0, 4, 5, 7, 6, 4].iter().map(|&k| c(k)).collect());
        for (a, b) in [(1, 5), (3, 7), (2, 6)] {
            out.push(vec![c(a), c(b)]);
        }
    };
    let mut lines = vec![];
    let (lo, hi) = cam_paths::stock_box(cam, part);
    boxes(lo, hi, &mut lines);
    let o = cam_paths::work_origin(cam, part);
    for (_, a, b) in &cam.fixtures.iter().map(|f| (0, DVec3::from(f.min) + o, DVec3::from(f.max) + o)).collect::<Vec<_>>() {
        boxes(a.min(*b), a.max(*b), &mut lines);
    }
    if let Some(j) = crate::cam_jaws::jaws(cam, part) {
        for (a, b) in j.rects {
            boxes(a.extend(j.bottom), b.extend(j.top), &mut lines);
        }
    }
    let s = (hi - lo).max_element() * 0.15;
    for d in [DVec3::X, DVec3::Y, DVec3::Z] {
        lines.push(vec![o, o + d * s]);
    }
    let items = [(&m, crate::assembly::Pose::default(), [0x9a, 0xa4, 0xb2])];
    crate::agent::raster_scene(if m.indices.is_empty() { &[] } else { &items }, &lines, "iso", None, w, h).ok()
}

/// The setup sheet as a self-contained HTML page.
pub fn html(cam: &Cam, part: &Part, paths: &[Toolpath], png: Option<&[u8]>) -> String {
    let o = cam_paths::work_origin(cam, part);
    let (lo, hi) = cam_paths::stock_box(cam, part);
    let total: f64 = paths.iter().map(|p| p.time).sum();
    let used: Vec<u64> = cam.tools.iter().map(|t| t.id).filter(|id| paths.iter().any(|p| p.tool == *id && !p.moves.is_empty())).collect();
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{name} - setup sheet</title><style>
body{{font:13px/1.45 system-ui,sans-serif;color:#1d2330;margin:24px;max-width:1100px}}
h1{{font-size:22px;margin:0 0 4px}} h2{{font-size:15px;margin:22px 0 6px;border-bottom:2px solid #1d2330;padding-bottom:2px}}
.muted{{color:#667}} table{{border-collapse:collapse;width:100%}} th,td{{text-align:left;padding:4px 8px;border-bottom:1px solid #d6dae2;vertical-align:top}}
th{{background:#eef1f5;font-weight:600}} td.n{{text-align:right;font-variant-numeric:tabular-nums}}
.grid{{display:grid;grid-template-columns:1fr 1fr;gap:18px;align-items:start}} .kv td:first-child{{color:#556;width:42%}}
img{{max-width:100%;border-radius:6px}} .warn{{color:#b3401a}} @media print{{body{{margin:0}} h2{{break-after:avoid}} tr{{break-inside:avoid}}}}
</style></head><body><h1>{name}</h1><div class="muted">Setup sheet · program {program} · {post}</div>"#,
        name = esc(&cam.name),
        program = cam.program,
        post = esc(crate::cam::POSTS.iter().find(|p| p.0 == cam.post).map(|p| p.1).unwrap_or(&cam.post)),
    );
    // setup summary and picture
    let w = |v: DVec3| v - o;
    let mut kv: Vec<(&str, String)> = vec![
        ("Up direction", if cam.tilt == [0.0; 2] {
            format!("part {} faces the spindle", cam.orient)
        } else {
            format!("part {} faces the spindle, tilted {}° about X then {}° about Y", cam.orient, fmt_mm(cam.tilt[0]), fmt_mm(cam.tilt[1]))
        }),
        ("Work zero (G54)", cam.origin.label().to_string()),
        ("Stock", format!("{} × {} × {} mm, {}", fmt_mm(hi.x - lo.x), fmt_mm(hi.y - lo.y), fmt_mm(hi.z - lo.z), cam.stock.material)),
        ("Stock from / to (work)", format!("{} / {}", xyz(w(lo)), xyz(w(hi)))),
        ("Part from / to (work)", format!("{} / {}", xyz(w(part.lo)), xyz(w(part.hi)))),
        ("Clearance / retract", format!("{} / {} mm", fmt_mm(cam.clearance), fmt_mm(cam.retract))),
        ("Machine", if cam.machine.name.is_empty() { "custom".into() } else { cam.machine.name.clone() }),
        ("Tool changes", if cam.machine.tool_changer { "automatic".into() } else { "by hand (the program stops)".into() }),
        ("Cycle time", time(total)),
        ("Tools / operations", format!("{} / {}", used.len(), paths.iter().filter(|p| !p.moves.is_empty()).count())),
    ];
    if let (Some(sj), Some(j)) = (&cam.soft_jaws, crate::cam_jaws::jaws(cam, part)) {
        kv.push((
            "Soft jaws",
            format!(
                "{} × {} × {} mm blanks, {} apart; part sits {} mm deep, jaw tops at Z{}",
                fmt_mm(sj.width),
                fmt_mm(sj.thickness),
                fmt_mm(sj.height),
                fmt_mm(sj.gap),
                fmt_mm(sj.grip),
                fmt_mm(j.top - o.z)
            ),
        ));
        if let Some(m) = &sj.mount {
            kv.push(("Jaw mounting", mount_text(m)));
        }
    }
    if !cam.jaws_of.is_empty() {
        kv.push(("Machines", format!("the soft jaws of {}: clamp both blanks in the vise on a spacer", cam.jaws_of)));
    }
    if !cam.follows.is_empty() {
        kv.insert(0, ("Starts from", format!("what {} leaves (flip the part)", cam.follows)));
    }
    match cam.probe.mode.as_str() {
        "plate" => kv.push((
            "Tool probing",
            format!(
                "touch plate {} mm thick at X{} Y{} (on Z{})",
                fmt_mm(cam.probe.plate_thickness),
                fmt_mm(cam.probe.plate_at[0]),
                fmt_mm(cam.probe.plate_at[1]),
                fmt_mm(cam.probe.plate_at[2])
            ),
        )),
        "setter" => kv.push(("Tool probing", "tool setter: zero the work Z with the first tool".into())),
        _ => {}
    }
    s.push_str("<h2>Setup</h2><div class=\"grid\"><table class=\"kv\">");
    for (k, v) in &kv {
        let _ = write!(s, "<tr><td>{}</td><td>{}</td></tr>", k, esc(v));
    }
    s.push_str("</table><div>");
    if let Some(png) = png {
        let _ = write!(s, "<img alt=\"The part on the machine\" src=\"data:image/png;base64,{}\">", base64::engine::general_purpose::STANDARD.encode(png));
        s.push_str("<div class=\"muted\">As on the machine (spindle above): outlines of the stock and fixtures, and the X, Y and Z axes from the work zero.</div>");
    }
    s.push_str("</div></div>");
    if !cam.fixtures.is_empty() {
        s.push_str("<h2>Fixtures</h2><table><tr><th>Name</th><th>From (work)</th><th>To (work)</th></tr>");
        for f in &cam.fixtures {
            let _ = write!(s, "<tr><td>{}</td><td>{}</td><td>{}</td></tr>", esc(&f.name), xyz(DVec3::from(f.min)), xyz(DVec3::from(f.max)));
        }
        s.push_str("</table>");
    }
    // tools
    s.push_str("<h2>Tools</h2><table><tr><th>T</th><th>Tool</th><th>Ø</th><th>Flutes</th><th>Flute length</th><th>Stick-out</th><th>Holder</th><th>Speed</th><th>Feed / plunge</th><th>Used by</th></tr>");
    for id in &used {
        let Some(t) = cam.tool(*id) else { continue };
        let ops: Vec<String> = paths.iter().filter(|p| p.tool == *id && !p.moves.is_empty()).filter_map(|p| cam.op(p.op)).map(|o| esc(&o.name)).collect();
        let desc = t.describe();
        let name = if t.name.is_empty() || t.name == desc { esc(&desc) } else { format!("{}<div class=\"muted\">{}</div>", esc(&t.name), esc(&desc)) };
        let holder = if t.holder_name.is_empty() { format!("Ø{} mm", fmt_mm(t.holder_diameter)) } else { esc(&t.holder_name) };
        let _ = write!(
            s,
            "<tr><td>T{}</td><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td>{}</td><td class=\"n\">{} rpm</td><td class=\"n\">{} / {}</td><td>{}</td></tr>",
            t.number,
            name,
            fmt_mm(t.diameter),
            t.flutes,
            fmt_mm(t.flute_length),
            fmt_mm(t.stickout),
            holder,
            t.rpm.round(),
            t.feed.round(),
            t.plunge_feed.round(),
            ops.join(", ")
        );
    }
    s.push_str("</table>");
    // operations
    s.push_str("<h2>Operations</h2><table><tr><th>#</th><th>Operation</th><th>T</th><th>Z top / bottom (work)</th><th>Stepdown / stepover</th><th>Leave</th><th>Cut length</th><th>Time</th><th>Notes</th></tr>");
    let mut n = 0;
    for tp in paths.iter().filter(|p| !p.moves.is_empty()) {
        let (Some(op), Some(t)) = (cam.op(tp.op), cam.tool(tp.tool)) else { continue };
        n += 1;
        let mut prev = DVec3::new(0.0, 0.0, hi.z);
        let (mut zlo, mut zhi) = (f64::INFINITY, f64::NEG_INFINITY);
        for m in &tp.moves {
            let p = m.end(prev);
            if matches!(m, Move::Feed(..) | Move::Tap { .. }) {
                zlo = zlo.min(p.z.min(prev.z));
                // moves above the stock are lifts over fixtures and lead-ins, not cuts
                if p.z <= hi.z + 1e-6 {
                    zhi = zhi.max(p.z);
                }
            }
            if let Move::Tap { z, .. } = m {
                zlo = zlo.min(*z);
            }
            prev = p;
        }
        let z = if zlo.is_finite() && zhi.is_finite() { format!("{} / {}", fmt_mm(zhi - o.z), fmt_mm(zlo - o.z)) } else { "-".into() };
        let step = if matches!(op.kind, OpKind::Drill | OpKind::Tap | OpKind::Thread | OpKind::Chamfer | OpKind::Deburr) {
            "-".to_string()
        } else {
            format!("{} mm / {}% Ø", fmt_mm(cam_paths::stepdown(op, t)), (cam_paths::stepover(op, t) / t.diameter.max(1e-9) * 100.0).round())
        };
        let mut notes: Vec<String> = vec![];
        if op.coolant || t.coolant {
            notes.push("coolant".into());
        }
        if op.rest {
            notes.push("rest machining".into());
        }
        notes.extend(tp.warnings.iter().map(|w| format!("<span class=\"warn\">{}</span>", esc(w))));
        let _ = write!(
            s,
            "<tr><td>{n}</td><td>{}<div class=\"muted\">{}</div></td><td>T{}</td><td class=\"n\">{z}</td><td class=\"n\">{step}</td><td class=\"n\">{}</td><td class=\"n\">{} mm</td><td class=\"n\">{}</td><td>{}</td></tr>",
            esc(&op.name),
            op.kind.label(),
            t.number,
            if op.leave > 0.0 || op.leave_floor > 0.0 { format!("{} / {}", fmt_mm(op.leave), fmt_mm(op.leave_floor)) } else { "-".into() },
            tp.cut_length.round(),
            time(tp.time),
            notes.join("; ")
        );
    }
    let _ = write!(s, "<tr><th></th><th>Total</th><th></th><th></th><th></th><th></th><th></th><th class=\"n\">{}</th><th></th></tr></table>", time(total));
    s.push_str("<p class=\"muted\">Heights are in work coordinates, relative to the work zero. Made by cad-rust-app.</p></body></html>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cam::ToolKind;

    #[test]
    fn sheet_lists_tools_and_operations() {
        let mut tris = vec![];
        let q = |x: f64, y: f64, z: f64| DVec3::new(x, y, z);
        for z in [0.0, 10.0] {
            tris.push([q(0.0, 0.0, z), q(40.0, 0.0, z), q(40.0, 30.0, z)]);
            tris.push([q(0.0, 0.0, z), q(40.0, 30.0, z), q(0.0, 30.0, z)]);
        }
        let part = Part::new(&tris, &[], "top");
        let mut c = Cam { name: "Plate <A>".into(), ..Default::default() };
        let t = crate::cam::presets().into_iter().find(|t| t.kind == ToolKind::Flat && t.diameter == 6.0).unwrap();
        let t = c.add_tool(t);
        c.add_op(OpKind::Face, t);
        c.add_op(OpKind::Contour, t);
        c.fixtures.push(Default::default());
        let paths = cam_paths::generate(&c, &part);
        let png = picture(&c, &part, 320, 240).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let h = html(&c, &part, &paths, Some(&png));
        assert!(h.contains("Plate &lt;A&gt;") && h.contains("data:image/png;base64,"));
        assert!(h.contains("Face 1") && h.contains("Contour 1") && h.contains(">T1<") && h.contains("Clamp"));
        assert!(h.contains("Ø6 flat end mill"));
    }
}

/// The screw holes the jaw blanks need, for the sheet.
pub fn mount_text(m: &crate::cam::JawMount) -> String {
    let xs: Vec<String> = m.holes.iter().map(|x| fmt_mm(*x)).collect();
    format!(
        "{}: {} × Ø{} through, Ø{} counterbore leaving {} mm under the head, {} mm from the grip face at X {} from the jaw middle",
        m.name,
        m.holes.len(),
        fmt_mm(m.drill),
        fmt_mm(m.cbore),
        fmt_mm(m.seat),
        fmt_mm(m.inset),
        xs.join(", ")
    )
}
