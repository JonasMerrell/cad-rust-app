//! Send posted G-code straight to rcnc, the CNC controller (github.com/JonasMerrell/rcnc), when it runs on
//! this computer: it listens on 127.0.0.1 and loads the program and its tools, so there is no file to save and open.

use std::time::Duration;

use serde_json::{Value, json};

use crate::cam::Cam;
use crate::cam_paths::Toolpath;

/// rcnc's local port (rcnc `crates/app/src/link.rs`).
pub const PORT: u16 = 7350;

/// The program and the tools it uses, as rcnc's `POST /program` takes them.
pub fn payload(cam: &Cam, paths: &[Toolpath], name: &str, gcode: String) -> Value {
    let mut tools: Vec<Value> = vec![];
    let mut seen: Vec<u64> = vec![];
    for p in paths.iter().filter(|p| !p.moves.is_empty()) {
        if seen.contains(&p.tool) {
            continue;
        }
        seen.push(p.tool);
        if let Some(t) = cam.tool(p.tool) {
            let d = t.describe();
            let description = if t.name.is_empty() || t.name == d { d } else { format!("{} - {d}", t.name) };
            tools.push(json!({"number": t.number, "diameter": t.diameter, "description": description}));
        }
    }
    json!({"name": name, "gcode": gcode, "tools": tools})
}

/// Send the program; Ok carries rcnc's message.
pub fn send(body: &Value) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{PORT}/program");
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(Duration::from_secs(20)).build();
    let message = |r: ureq::Response| r.into_string().ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["message"].as_str().map(String::from));
    match agent.post(&url).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => Ok(message(r).unwrap_or_else(|| "Sent to rcnc".into())),
        Err(ureq::Error::Status(_, r)) => Err(message(r).unwrap_or_else(|| "rcnc refused the program".into())),
        Err(ureq::Error::Transport(_)) => Err("rcnc isn't running on this computer: start it, then send again".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn sends_program_json() {
        // A one-shot stand-in for rcnc on its port (skipped if rcnc itself is running).
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", PORT)) else { return };
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // Headers and body can arrive separately: read until the whole body is in.
            let mut req = String::new();
            let mut buf = [0; 4096];
            loop {
                let n = s.read(&mut buf).unwrap();
                req.push_str(&String::from_utf8_lossy(&buf[..n]));
                let Some(head) = req.find("\r\n\r\n") else { continue };
                let len = req[..head].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                if n == 0 || req.len() >= head + 4 + len {
                    break;
                }
            }
            let body = r#"{"ok":true,"message":"Loaded part.nc in rcnc"}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            req
        });
        let body = json!({"name": "part.nc", "gcode": "G0 X1\nM30", "tools": [{"number": 1, "diameter": 6.0, "description": "6 mm flat"}]});
        assert_eq!(send(&body).unwrap(), "Loaded part.nc in rcnc");
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /program HTTP/1.1"), "{req}");
        assert!(req.contains("\"diameter\":6.0"), "{req}");
    }
}
