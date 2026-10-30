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
