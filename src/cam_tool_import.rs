//! Import tool libraries: Fusion 360 (.json export or .tools, a zip holding tools.json) and this
//! app's own tools.json.

use crate::cam::{fmt_mm, Tool, ToolKind};
use serde_json::Value;

/// What an import found: the tools, and a note per entry that was skipped.
#[derive(Default, Debug)]
pub struct Imported {
    pub tools: Vec<Tool>,
    pub skipped: Vec<String>,
}

pub fn import_file(path: &std::path::Path) -> Result<Imported, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = if bytes.starts_with(b"PK") {
        let names = ["tools.json"];
        unzip(&bytes, |n| names.contains(&n.rsplit('/').next().unwrap_or(n)) || n.ends_with(".json"))?
    } else {
        String::from_utf8(bytes).map_err(|_| "not a text or zip file".to_string())?
    };
    import_json(&text)
}

/// Add tools to the user's library (~/.config/cad-rust-app/tools.json), replacing ones with the same
/// name. Returns how many were new.
pub fn add_to_library(tools: &[Tool]) -> usize {
    let mut lib = crate::cam::load_library();
    let mut new = 0;
    for t in tools {
        match lib.iter_mut().find(|o| o.name == t.name) {
            Some(o) => *o = t.clone(),
            None => {
                lib.push(t.clone());
                new += 1;
            }
        }
    }
    crate::cam::save_library(&lib);
    new
}

pub fn import_json(text: &str) -> Result<Imported, String> {
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("not JSON: {e}"))?;
    // this app's own library: a list of tools
    if let Some(list) = v.as_array() {
        if list.iter().all(|t| t.get("kind").is_some()) {
            let tools: Vec<Tool> = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
            return Ok(Imported { tools, skipped: vec![] });
        }
    }
    let list = v.get("data").and_then(|d| d.as_array()).or(v.as_array()).ok_or("no tools in the file (expected a Fusion 360 tool library with a \"data\" list)")?;
    let mut out = Imported::default();
    for (i, e) in list.iter().enumerate() {
        match fusion_tool(e) {
            Ok(Some(t)) => out.tools.push(t),
            Ok(None) => {}
            Err(why) => out
                .skipped
                .push(format!("{}: {why}", e.get("description").and_then(|d| d.as_str()).filter(|d| !d.is_empty()).map(str::to_string).unwrap_or(format!("tool {}", i + 1)))),
        }
    }
    Ok(out)
}

/// One Fusion 360 tool. Ok(None) for entries that aren't tools (holders, probes).
fn fusion_tool(e: &Value) -> Result<Option<Tool>, String> {
    let ty = e.get("type").and_then(|t| t.as_str()).unwrap_or("").to_lowercase();
    if matches!(ty.as_str(), "holder" | "probe" | "") {
        return Ok(None);
    }
    let kind = match ty.as_str() {
        "flat end mill" => ToolKind::Flat,
        "ball end mill" | "lollipop mill" => ToolKind::Ball,
        "bull nose end mill" | "radius mill" => ToolKind::Bull,
        "tapered mill" => ToolKind::TaperedBall,
        "chamfer mill" | "counter sink" | "engrave" | "engraving" => ToolKind::Chamfer,
        "face mill" => ToolKind::FaceMill,
        "slot mill" | "dovetail mill" => ToolKind::Slot,
        "thread mill" => ToolKind::ThreadMill,
        "drill" => ToolKind::Drill,
        "spot drill" => ToolKind::SpotDrill,
        "center drill" => ToolKind::CenterDrill,
        "tap right hand" | "tap left hand" | "tap" => ToolKind::Tap,
        "reamer" => ToolKind::Reamer,
        "counter bore" => ToolKind::Flat,
        _ => return Err(format!("{ty} isn't supported")),
    };
    let unit = e.get("unit").and_then(|u| u.as_str()).unwrap_or("millimeters");
    let k = if unit.starts_with("inch") { 25.4 } else { 1.0 };
    let g = e.get("geometry").cloned().unwrap_or(Value::Null);
    let num = |keys: &[&str]| keys.iter().find_map(|key| g.get(*key).and_then(|v| v.as_f64()));
    let len = |keys: &[&str]| num(keys).map(|v| v * k);
    let d = len(&["DC"]).filter(|d| *d > 0.0).ok_or("no diameter (DC)")?;
    let mut t = Tool { kind, diameter: d, ..Default::default() };
    t.flute_length = len(&["LCF"]).filter(|v| *v > 0.0).unwrap_or(d * 3.0);
    t.shank_diameter = len(&["SFDM"]).filter(|v| *v > 0.0).unwrap_or(d);
    t.stickout = len(&["LB", "OAL"]).filter(|v| *v > 0.0).map(|v| if num(&["LB"]).is_some() { v } else { v * 0.6 }).unwrap_or(t.flute_length + 10.0).max(t.flute_length);
    t.flutes = num(&["NOF"]).map(|v| v.round().max(1.0) as u32).unwrap_or(2);
    t.corner_radius = len(&["RE"]).unwrap_or(0.0);
    t.tip_diameter = len(&["tip-diameter"]).unwrap_or(0.0);
    // Fusion keeps the taper angle per side; chamfer mills here use the included angle
    let ta = num(&["TA"]);
    match kind {
        ToolKind::Ball => t.corner_radius = d / 2.0,
        ToolKind::Chamfer => t.angle = ta.map(|a| 2.0 * a).filter(|a| *a > 0.0).unwrap_or(90.0),
        ToolKind::TaperedBall => {
            t.angle = ta.unwrap_or(5.0);
            if t.corner_radius <= 0.0 {
                t.corner_radius = (t.tip_diameter / 2.0).max(0.25);
            }
        }
        ToolKind::Drill | ToolKind::SpotDrill | ToolKind::CenterDrill => t.angle = num(&["SIG"]).filter(|a| *a > 0.0).unwrap_or(if kind == ToolKind::Drill { 118.0 } else { 90.0 }),
        ToolKind::Slot => {
            t.cutting_width = t.flute_length.min(d);
            t.neck_diameter = len(&["shoulder-diameter", "DN"]).unwrap_or(t.shank_diameter.min(d * 0.6));
        }
        ToolKind::ThreadMill => {
            t.cutting_width = t.flute_length;
            t.neck_diameter = len(&["shoulder-diameter", "DN"]).unwrap_or(d * 0.7);
        }
        _ => {}
    }
    if matches!(kind, ToolKind::Tap | ToolKind::ThreadMill) {
        t.pitch = len(&["TP", "thread-pitch"]).filter(|p| *p > 0.0).unwrap_or_else(|| coarse_pitch(d));
    }
    if kind == ToolKind::Bull && t.corner_radius <= 0.0 {
        t.kind = ToolKind::Flat;
    }
    t.material = match e.get("BMC").and_then(|m| m.as_str()).unwrap_or("carbide").to_lowercase().as_str() {
        "hss" => "hss",
        "cobalt" | "hss-co" => "cobalt",
        _ => "carbide",
    }
    .into();
    if let Some(n) = e.pointer("/post-process/number").and_then(|v| v.as_u64()) {
        t.number = n.max(1) as u32;
    }
    // holder: segments from its bottom face up
    if let Some(segs) = e.pointer("/holder/segments").and_then(|s| s.as_array()).filter(|s| !s.is_empty()) {
        let mut h = 0.0;
        let mut outline = vec![];
        for s in segs {
            let get = |key: &str| s.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0) * k;
            outline.push([get("lower-diameter"), h]);
            h += get("height");
            outline.push([get("upper-diameter"), h]);
        }
        if outline.iter().any(|p| p[0] > 0.0) {
            t.holder_diameter = outline.iter().map(|p| p[0]).fold(0.0, f64::max);
            t.holder = outline;
            t.holder_name = e.pointer("/holder/description").and_then(|d| d.as_str()).unwrap_or("").to_string();
        }
    }
    // the first feeds-and-speeds preset
    if let Some(p) = e.pointer("/start-values/presets/0") {
        let f = |key: &str| p.get(key).and_then(|v| v.as_f64()).filter(|v| *v > 0.0);
        if let Some(n) = f("n") {
            t.rpm = n;
        }
        if let Some(v) = f("v_f") {
            t.feed = v * k;
        }
        if let Some(v) = f("v_f_plunge") {
            t.plunge_feed = v * k;
        }
        if let Some(v) = f("stepdown") {
            t.stepdown = v * k;
        }
        if let Some(v) = f("stepover") {
            t.stepover = (v * k / d).clamp(0.01, 1.0);
        }
        // "disabled" is Fusion's default: leave it to the setup's coolant
        t.coolant = match p.get("tool-coolant").and_then(|c| c.as_str()) {
            Some(c) if c.contains("mist") || c.contains("air") => crate::cam::Coolant::Mist,
            Some(c) if c != "disabled" => crate::cam::Coolant::Flood,
            _ => crate::cam::Coolant::Auto,
        };
    } else {
        // no feeds: suggested when the tool is added to a setup
        t.rpm = 0.0;
    }
    if kind == ToolKind::Tap && t.rpm > 0.0 {
        t.feed = t.rpm * t.pitch;
        t.plunge_feed = t.feed;
    }
    let desc = e.get("description").and_then(|d| d.as_str()).unwrap_or("").trim();
    let vendor = e.get("vendor").and_then(|d| d.as_str()).unwrap_or("").trim();
    t.name = if desc.is_empty() { t.describe() } else { desc.to_string() };
    if !vendor.is_empty() && !t.name.contains(vendor) {
        t.name = format!("{} ({vendor})", t.name);
    }
    if k != 1.0 && desc.is_empty() {
        t.name = format!("{} (Ø{} in)", t.name, fmt_mm(d / 25.4));
    }
    Ok(Some(t))
}

/// ISO coarse pitch for a metric size.
fn coarse_pitch(d: f64) -> f64 {
    const P: [(f64, f64); 10] = [(2.0, 0.4), (2.5, 0.45), (3.0, 0.5), (4.0, 0.7), (5.0, 0.8), (6.0, 1.0), (8.0, 1.25), (10.0, 1.5), (12.0, 1.75), (16.0, 2.0)];
    P.iter().min_by(|a, b| (a.0 - d).abs().total_cmp(&(b.0 - d).abs())).map(|p| p.1).unwrap_or(1.0)
}

/// The first entry of a zip archive whose name `want`s, as text.
fn unzip(b: &[u8], want: impl Fn(&str) -> bool) -> Result<String, String> {
    use std::io::Read;
    let u16_at = |i: usize| b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize).ok_or("broken zip");
    let u32_at = |i: usize| b.get(i..i + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize).ok_or("broken zip");
    // end of central directory
    let eocd = (0..b.len().saturating_sub(21)).rev().find(|&i| b[i..].starts_with(b"PK\x05\x06")).ok_or("broken zip: no directory")?;
    let count = u16_at(eocd + 10)?;
    let mut at = u32_at(eocd + 16)?;
    for _ in 0..count {
        if !b.get(at..).map(|s| s.starts_with(b"PK\x01\x02")).unwrap_or(false) {
            return Err("broken zip directory".into());
        }
        let method = u16_at(at + 10)?;
        let size = u32_at(at + 20)?;
        let (nl, xl, cl) = (u16_at(at + 28)?, u16_at(at + 30)?, u16_at(at + 32)?);
        let local = u32_at(at + 42)?;
        let name = String::from_utf8_lossy(b.get(at + 46..at + 46 + nl).ok_or("broken zip")?).to_string();
        at += 46 + nl + xl + cl;
        if !want(&name) {
            continue;
        }
        let data = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
        let raw = b.get(data..data + size).ok_or("broken zip entry")?;
        let mut out = String::new();
        match method {
            0 => out = String::from_utf8_lossy(raw).to_string(),
            8 => {
                flate2::read::DeflateDecoder::new(raw).read_to_string(&mut out).map_err(|e| format!("{name}: {e}"))?;
            }
            m => return Err(format!("{name}: unsupported zip compression {m}")),
        }
        return Ok(out);
    }
    Err("no tools.json in the archive".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"{"data":[
      {"type":"flat end mill","unit":"millimeters","description":"6mm 3FL","vendor":"Acme","BMC":"carbide",
       "geometry":{"DC":6,"LCF":18,"LB":25,"OAL":50,"SFDM":6,"NOF":3},
       "post-process":{"number":4},
       "holder":{"description":"ER20","segments":[{"height":20,"lower-diameter":32,"upper-diameter":34},{"height":30,"lower-diameter":40,"upper-diameter":40}]},
       "start-values":{"presets":[{"n":18000,"v_f":1500,"v_f_plunge":400,"stepdown":3,"stepover":2.4,"tool-coolant":"flood"}]}},
      {"type":"chamfer mill","unit":"inches","description":"","geometry":{"DC":0.5,"TA":45,"LCF":0.4,"SFDM":0.5,"NOF":2,"tip-diameter":0.0}},
      {"type":"tap right hand","unit":"millimeters","description":"M6 tap","geometry":{"DC":6,"TP":1,"LCF":12},"start-values":{"presets":[{"n":500}]}},
      {"type":"holder","description":"a holder"},
      {"type":"boring bar","description":"Boring bar"}
    ]}"#;

    #[test]
    fn reads_fusion_libraries() {
        let r = import_json(LIB).unwrap();
        assert_eq!(r.tools.len(), 3, "{:?}", r.skipped);
        assert_eq!(r.skipped.len(), 1);
        let t = &r.tools[0];
        assert_eq!((t.kind, t.number, t.flutes), (ToolKind::Flat, 4, 3));
        assert_eq!((t.diameter, t.flute_length, t.stickout, t.rpm, t.feed, t.plunge_feed), (6.0, 18.0, 25.0, 18000.0, 1500.0, 400.0));
        assert!((t.stepover - 0.4).abs() < 1e-9 && t.coolant == crate::cam::Coolant::Flood);
        assert_eq!(t.holder, vec![[32.0, 0.0], [34.0, 20.0], [40.0, 20.0], [40.0, 50.0]]);
        assert_eq!(t.name, "6mm 3FL (Acme)");
        let c = &r.tools[1];
        assert_eq!((c.kind, c.angle, c.diameter, c.rpm), (ToolKind::Chamfer, 90.0, 12.7, 0.0));
        let tap = &r.tools[2];
        assert_eq!((tap.kind, tap.pitch, tap.feed), (ToolKind::Tap, 1.0, 500.0));
    }

    #[test]
    fn reads_zipped_tools_files() {
        use std::io::Write;
        let mut z = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::default());
        z.write_all(LIB.as_bytes()).unwrap();
        let data = z.finish().unwrap();
        // a one-entry zip
        let name = b"tools.json";
        let mut b = vec![];
        let local = b.len();
        b.extend(b"PK\x03\x04");
        b.extend([20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        b.extend((data.len() as u32).to_le_bytes());
        b.extend((LIB.len() as u32).to_le_bytes());
        b.extend((name.len() as u16).to_le_bytes());
        b.extend([0, 0]);
        b.extend(name);
        b.extend(&data);
        let cd = b.len();
        b.extend(b"PK\x01\x02");
        b.extend([20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        b.extend((data.len() as u32).to_le_bytes());
        b.extend((LIB.len() as u32).to_le_bytes());
        b.extend((name.len() as u16).to_le_bytes());
        b.extend([0; 12]);
        b.extend((local as u32).to_le_bytes());
        b.extend(name);
        let cd_len = b.len() - cd;
        b.extend(b"PK\x05\x06");
        b.extend([0, 0, 0, 0, 1, 0, 1, 0]);
        b.extend((cd_len as u32).to_le_bytes());
        b.extend((cd as u32).to_le_bytes());
        b.extend([0, 0]);
        let text = unzip(&b, |n| n == "tools.json").unwrap();
        assert_eq!(import_json(&text).unwrap().tools.len(), 3);
    }
}
