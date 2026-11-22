//! The command line and the MCP server end to end, run as agents and CI run them.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_rcad")
}

/// An empty folder to run in.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cad-agent-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn version_and_usage() {
    let o = Command::new(exe()).arg("--version").output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains(env!("CARGO_PKG_VERSION")));
    let o = Command::new(exe()).arg("--help").output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("run [--stop-on-error] script.json"));
    let o = Command::new(exe()).arg("--bogus").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown option --bogus"));
}

/// A fillet too big for its edge: `run` reports that step as failed and exits 1, the rest of
/// the script still runs, and the outputs land in the working directory.
#[test]
fn regression_stand_reports_the_failing_fillet() {
    let dir = scratch("stand");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/regression-stand.json");
    let o = Command::new(exe()).arg("run").arg(&script).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", String::from_utf8_lossy(&o.stderr));
    let lines: Vec<Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 10);
    let failed: Vec<u64> = lines.iter().filter(|l| l.get("error").is_some()).map(|l| l["step"].as_u64().unwrap()).collect();
    assert_eq!(failed, vec![6]);
    assert_eq!(lines[6]["result"]["ok"], json!(false));
    assert!(lines[6]["error"].as_str().unwrap().contains("Fillet 2"));
    assert!(dir.join("regression.png").exists() && dir.join("stand.stl").exists());

    // stopping at the first failure
    let o = Command::new(exe()).args(["run", "--stop-on-error"]).arg(&script).current_dir(&dir).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 7);
}

struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(dir: &Path) -> Mcp {
        let mut child = Command::new(exe()).arg("mcp").current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Mcp { child, out, id: 0 }
    }

    fn line(&mut self, text: &str) -> Value {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{text}").unwrap();
        stdin.flush().unwrap();
        let mut l = String::new();
        self.out.read_line(&mut l).unwrap();
        serde_json::from_str(&l).unwrap()
    }

    fn req(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        self.line(&json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}).to_string())
    }

    /// (isError, the result's text)
    fn call(&mut self, name: &str, args: Value) -> (bool, String) {
        let r = self.req("tools/call", json!({"name": name, "arguments": args}));
        (r["result"]["isError"] == json!(true), r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_protocol_and_errors() {
    let dir = scratch("mcp");
    let mut m = Mcp::start(&dir);
    let r = m.line("this is not json");
    assert_eq!(r["error"]["code"], json!(-32700));
    assert_eq!(r["id"], Value::Null);
    let r = m.req("initialize", json!({"protocolVersion": "2024-11-05", "capabilities": {}}));
    assert_eq!(r["result"]["protocolVersion"], json!("2024-11-05"));
    let r = m.req("initialize", json!({"protocolVersion": "1999-01-01", "capabilities": {}}));
    assert_eq!(r["result"]["protocolVersion"], json!("2025-06-18"));

    let r = m.req("tools/list", json!({}));
    for t in r["result"]["tools"].as_array().unwrap() {
        for (k, p) in t["inputSchema"]["properties"].as_object().unwrap() {
            assert!(p.get("description").is_some(), "{}.{k} has no description", t["name"]);
        }
    }

    assert!(!m.call("new_document", json!({})).0);
    assert!(!m.call("create_sketch", json!({"plane": "top"})).0);
    assert!(!m.call("sketch_add", json!({"sketch_id": 1, "items": [{"type": "rect", "corner1": [0, 0], "corner2": [40, 20]}]})).0);
    // typos and bad values are errors that say what was meant
    let (err, text) = m.call("add_feature", json!({"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "dept": 30}}));
    assert!(err && text.contains("did you mean 'depth'"), "{text}");
    let (err, text) = m.call("sketch_constrain", json!({"sketch_id": 1, "constraints": [{"type": "length", "l": 999, "value": 5}]}));
    assert!(err && text.contains("999"), "{text}");
    let (err, text) = m.call("sketch_add", json!({"sketch_id": 1, "items": [{"type": "circle", "center": [20, 10], "radius": -3}]}));
    assert!(err && text.contains("radius"), "{text}");
    assert!(m.call("set_variable", json!({"name": "x", "expr": "1 +"})).0);
    assert!(m.call("render", json!({"view": "sideways"})).0);

    let (err, text) = m.call("add_feature", json!({"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 10}}));
    assert!(!err, "{text}");
    // compact JSON
    assert!(!text.contains('\n'));
    // a fillet that can't be built: an error, and with rollback the call is undone
    let fillet = json!({"edges": [{"kind": "edge", "point": [40, 10, 10]}], "radius": 50});
    let (err, text) = m.call("add_feature", json!({"kind": "fillet", "params": fillet, "rollback": true}));
    assert!(err && text.contains("new_errors") && text.contains("rolled_back"), "{text}");
    let (_, text) = m.call("describe", json!({}));
    let d: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(d["features"].as_array().unwrap().len(), 2);
    let (err, text) = m.call("add_feature", json!({"kind": "fillet", "params": fillet}));
    assert!(err && text.contains("new_errors"), "{text}");

    // exports create their folder and errors name the path
    let (err, text) = m.call("export", json!({"format": "stl", "path": "out/part.stl"}));
    assert!(!err, "{text}");
    assert!(dir.join("out/part.stl").exists());
    let (err, text) = m.call("open_document", json!({"path": "missing.json"}));
    assert!(err && text.contains("can't open missing.json"), "{text}");
}

/// A CAM call that fails part way leaves the Manufacture tab as it was: no half-made operation
/// (the operation used to be added before its sketch was looked up).
#[test]
fn failed_cam_op_adds_nothing() {
    let dir = scratch("camop");
    let script = dir.join("cam.json");
    let steps = json!([
        {"tool": "create_sketch", "args": {"plane": "top"}},
        {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [{"type": "rect", "corner1": [0, 0], "corner2": [40, 30]}]}},
        {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 10}}},
        {"tool": "create_cam", "args": {}},
        {"tool": "cam_op", "args": {"cam": 0, "kind": "trace", "sketches": ["no such sketch"]}},
        {"tool": "cam_info", "args": {"cam": 0}}
    ]);
    std::fs::write(&script, steps.to_string()).unwrap();
    let o = Command::new(exe()).arg("run").arg(&script).current_dir(&dir).output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 6, "{}", String::from_utf8_lossy(&o.stderr));
    for l in &lines[..4] {
        assert!(l.get("error").is_none(), "{l}");
    }
    assert!(lines[4]["error"].as_str().unwrap().contains("no sketch"), "{}", lines[4]);
    assert_eq!(lines[5]["result"]["operations"], json!([]), "{}", lines[5]);
}

/// CAM builds are cached between calls, and the cache follows every change: an operation edited, the part
/// changed, or the setup a following setup starts from changed.
#[test]
fn cam_builds_follow_every_change() {
    let dir = scratch("camcache");
    let script = dir.join("cam.json");
    let steps = json!([
        {"tool": "create_sketch", "args": {"plane": "top"}},
        {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [{"type": "rect", "corner1": [0, 0], "corner2": [40, 30]}]}},
        {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 10}}},
        {"tool": "create_cam", "args": {"stock": {"side": 3, "top": 2}}},
        {"tool": "cam_tool", "args": {"cam": 0, "kind": "flat", "diameter": 6, "number": 1}},
        {"tool": "cam_op", "args": {"cam": 0, "kind": "contour", "tool": 1}},
        {"tool": "cam_info", "args": {"cam": 0}},
        {"tool": "cam_info", "args": {"cam": 0}},
        {"tool": "cam_op", "args": {"cam": 0, "id": 2, "stepdown": 1.0}},
        {"tool": "cam_info", "args": {"cam": 0}},
        {"tool": "create_sketch", "args": {"plane": "top"}},
        {"tool": "sketch_add", "args": {"sketch_id": 3, "items": [{"type": "rect", "corner1": [0, 0], "corner2": [60, 30]}]}},
        {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 3}], "depth": 10}}},
        {"tool": "cam_info", "args": {"cam": 0}},
        {"tool": "create_cam", "args": {"name": "Op 2", "follows": "Manufacture 1", "orient": "bottom"}},
        {"tool": "cam_op", "args": {"cam": 1, "kind": "adaptive", "tool": 1}},
        {"tool": "cam_info", "args": {"cam": 1}},
        {"tool": "cam_op", "args": {"cam": 0, "id": 2, "delete": true}},
        {"tool": "cam_info", "args": {"cam": 1}}
    ]);
    std::fs::write(&script, steps.to_string()).unwrap();
    let o = Command::new(exe()).arg("run").arg(&script).current_dir(&dir).output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 19, "{}", String::from_utf8_lossy(&o.stderr));
    for l in &lines {
        assert!(l.get("error").is_none(), "{l}");
    }
    let info = |i: usize| &lines[i]["result"];
    let op = |i: usize| info(i)["operations"][0].clone();
    // the same build twice
    assert_eq!(op(6), op(7));
    // an edited operation: more passes, so more moves
    assert!(op(9)["moves"].as_u64().unwrap() > op(7)["moves"].as_u64().unwrap(), "{} {}", op(7), op(9));
    // a longer part: a longer contour
    assert!(op(13)["cut_length"].as_f64().unwrap() > op(9)["cut_length"].as_f64().unwrap() + 30.0, "{} {}", op(9), op(13));
    // the setup that follows starts from what op 1 leaves: with op 1's contour gone there is more to clear
    let rough = |i: usize| info(i)["operations"][0]["cut_length"].as_f64().unwrap();
    assert!(rough(18) > rough(16) + 1.0, "{} {}", rough(16), rough(18));
}

/// The simulation is shared between simulate_cam and the export gate, and verify_gcode still reads the posted file:
/// a program edited after it was exported is caught, the untouched one passes.
#[test]
fn shared_simulation_still_catches_an_edited_program() {
    let dir = scratch("simcache");
    let steps = |extra: Value| {
        let mut v = json!([
            {"tool": "create_sketch", "args": {"plane": "top"}},
            {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [{"type": "rect", "corner1": [0, 0], "corner2": [40, 30]}]}},
            {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 10}}},
            {"tool": "create_cam", "args": {"stock": {"side": 3, "top": 2}}},
            {"tool": "cam_tool", "args": {"cam": 0, "kind": "flat", "diameter": 6, "number": 1}},
            {"tool": "cam_op", "args": {"cam": 0, "kind": "contour", "tool": 1}}
        ]);
        v.as_array_mut().unwrap().extend(extra.as_array().unwrap().iter().cloned());
        v
    };
    let run = |name: &str, v: Value| -> Vec<Value> {
        let script = dir.join(name);
        std::fs::write(&script, v.to_string()).unwrap();
        let o = Command::new(exe()).arg("run").arg(&script).current_dir(&dir).output().unwrap();
        String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    };
    let a = run("a.json", steps(json!([
        {"tool": "simulate_cam", "args": {"cam": 0}},
        {"tool": "export_gcode", "args": {"cam": 0, "path": "p.nc"}},
        {"tool": "simulate_cam", "args": {"cam": 0}},
        {"tool": "verify_gcode", "args": {"cam": 0, "path": "p.nc"}},
        {"tool": "cam_info", "args": {"cam": 0}}
    ])));
    assert_eq!(a.len(), 11, "{a:?}");
    // the same run both times, and the export gate passed on it
    assert_eq!(a[6]["result"]["problems"], a[8]["result"]["problems"]);
    assert_eq!(a[6]["result"]["removed_mm3"], a[8]["result"]["removed_mm3"]);
    assert_eq!(a[7]["result"]["sim"]["ok"], json!(true), "{}", a[7]);
    assert_eq!(a[9]["result"]["ok"], json!(true), "{}", a[9]);
    // edit the posted program: after the first cutting move, a move through the middle of the part at that depth
    let info = &a[10]["result"];
    let (lo, hi) = (&info["part"]["min"], &info["part"]["max"]);
    let mid = |k: usize| (lo[k].as_f64().unwrap() + hi[k].as_f64().unwrap()) / 2.0;
    let text = std::fs::read_to_string(dir.join("p.nc")).unwrap();
    let top = hi[2].as_f64().unwrap();
    let mut done = false;
    let mut z = f64::INFINITY;
    let mut edited = vec![];
    for l in text.lines() {
        edited.push(l.to_string());
        if let Some(v) = l.split_whitespace().find_map(|w| w.strip_prefix('Z')).and_then(|v| v.parse::<f64>().ok()) {
            z = v;
        }
        if !done && z < top - 1.0 && l.starts_with("G1 ") && (l.contains('X') || l.contains('Y')) {
            edited.push(format!("G1 X{:.3} Y{:.3}", mid(0), mid(1)));
            done = true;
        }
    }
    assert!(done);
    std::fs::write(dir.join("p.nc"), edited.join("\n")).unwrap();
    let b = run("b.json", steps(json!([
        {"tool": "simulate_cam", "args": {"cam": 0}},
        {"tool": "verify_gcode", "args": {"cam": 0, "path": "p.nc"}}
    ])));
    assert_eq!(b[7]["result"]["ok"], json!(false), "{}", b[7]);
    assert!(!b[7]["result"]["problems"].as_array().unwrap().is_empty(), "{}", b[7]);
}

/// A Rhai script builds a feature tree, not baked solids: the saved document has the variables,
/// dimensioned sketches and features, and changing a variable resizes the part when it reopens.
#[test]
fn script_builds_an_editable_feature_tree() {
    let dir = scratch("script-doc");
    std::fs::write(
        dir.join("plate.rhai"),
        r#"
let doc = new_document();
doc.variable("w", 60);
doc.variable("t", 8);
let base = doc.sketch("top", "Plate");
base.rect([0, 0], "w", 40);
doc.extrude(base, "t");
let holes = doc.sketch(doc.faces(#{normal: [0, 0, 1]})[0], "Holes");
for x in [-20, 20] { holes.circle([x, 0], 6); }
doc.extrude(holes, 10, #{op: "remove", end: "through_all"});
doc.fillet(doc.edges().filter(|e| e.type == "line" && e.start[0] == e.end[0] && e.start[1] == e.end[1]), 3);
let failed = false;
try { doc.fillet(doc.edges().filter(|e| e.type == "line"), 50); } catch { failed = true; }
doc.save("plate.json");
doc.export_file("plate.step");
doc.render("plate.png", "iso");
#{parts: doc.parts(), failed: failed, features: doc.describe().features.len(), tool: doc.tool("feature_defaults", #{kind: "hole"}).kind}
"#,
    )
    .unwrap();
    let o = Command::new(exe()).args(["script", "plate.rhai"]).current_dir(&dir).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out: Value = serde_json::from_str(String::from_utf8_lossy(&o.stdout).lines().last().unwrap()).unwrap();
    // the too-big fillet threw and was taken out again
    assert_eq!(out["failed"], json!(true));
    assert_eq!(out["features"], json!(5));
    // any MCP tool through `tool` (Rhai keeps `call` for function pointers)
    assert_eq!(out["tool"], json!("hole"));
    let vol = out["parts"][0]["volume_mm3"].as_f64().unwrap();
    let hole = std::f64::consts::PI * 9.0 * 8.0;
    let corners = 4.0 * (9.0 - std::f64::consts::PI * 9.0 / 4.0) * 8.0;
    assert!((vol - (60.0 * 40.0 * 8.0 - 2.0 * hole - corners)).abs() < 1.0, "{vol}");

    assert!(dir.join("plate.step").exists() && dir.join("plate.png").exists());
    let saved = std::fs::read_to_string(dir.join("plate.json")).unwrap();
    for want in ["\"Plate\"", "\"Holes\"", "\"expr\": \"w\"", "\"depth\": \"t\"", "\"diameter\"", "Fillet"] {
        assert!(saved.contains(want) || saved.replace(' ', "").contains(&want.replace(' ', "")), "saved document lacks {want}");
    }

    // editing the variable resizes the reopened part, as it would in the editor
    std::fs::write(dir.join("wider.rhai"), "let d = open_document(\"plate.json\"); d.variable(\"w\", 80); d.parts()[0].bbox_mm").unwrap();
    let o = Command::new(exe()).args(["script", "wider.rhai"]).current_dir(&dir).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let bbox: Value = serde_json::from_str(String::from_utf8_lossy(&o.stdout).lines().last().unwrap()).unwrap();
    let w = bbox[1][0].as_f64().unwrap() - bbox[0][0].as_f64().unwrap();
    assert!((w - 80.0).abs() < 1e-3, "{bbox}");

    // a relative document is also found next to the script, run from another folder
    let o = Command::new(exe()).args(["script", &format!("{}/wider.rhai", dir.file_name().unwrap().to_string_lossy())]).current_dir(dir.parent().unwrap()).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// sketch_add `dims` stores driving dimensions, and their expressions resize the geometry.
#[test]
fn sketch_add_dims_drive_the_geometry() {
    let dir = scratch("dims");
    let steps = json!([
        {"tool": "set_variable", "args": {"name": "d", "expr": "12"}},
        {"tool": "create_sketch", "args": {"plane": "top"}},
        {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [
            {"type": "center_rect", "center": [0, 0], "corner": [10, 10], "dims": {"width": "50", "height": "d * 2"}},
            {"type": "circle", "center": [0, 0], "radius": 3, "dims": {"diameter": "d"}},
            {"type": "line", "from": [30, 0], "to": [40, 0], "dims": true, "construction": true},
            {"type": "line", "from": [30, 5], "to": [42.819394117, 5], "dims": true, "construction": true}
        ]}},
        {"tool": "add_feature", "args": {"kind": "extrude", "params": {"profiles": [{"sketch": 1}], "depth": 5}}},
        {"tool": "sketch_add", "args": {"sketch_id": 1, "items": [{"type": "ellipse", "center": [0, 0], "major_end": [5, 0], "minor_radius": 2, "dims": true}]}},
        {"tool": "save_document", "args": {"path": "dims.json"}}
    ]);
    std::fs::write(dir.join("dims-run.json"), steps.to_string()).unwrap();
    let o = Command::new(exe()).args(["run", "dims-run.json"]).current_dir(&dir).output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&o.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(lines[2].get("error").is_none(), "{}", lines[2]);
    assert_eq!(lines[2]["result"]["created"][0]["dims"], json!(["width 50", "height d * 2"]));
    assert_eq!(lines[2]["result"]["created"][2]["dims"], json!(["length 10"]));
    // a size that isn't round (from imported geometry, say) keeps every digit, so solving doesn't move it
    assert_eq!(lines[2]["result"]["created"][3]["dims"], json!(["length 12.819394117"]));
    let part = &lines[3]["result"]["parts"][0];
    assert_eq!(part["bbox_mm"], json!([[-25.0, -12.0, 0.0], [25.0, 12.0, 5.0]]), "{part}");
    let vol = 50.0 * 24.0 * 5.0 - std::f64::consts::PI * 36.0 * 5.0;
    assert!((part["volume_mm3"].as_f64().unwrap() - vol).abs() < 0.5, "{part}");
    // an item without dimensionable sizes says which ones have them
    assert!(lines[4]["error"].as_str().unwrap().contains("ellipse takes no dims"), "{}", lines[4]);
}
