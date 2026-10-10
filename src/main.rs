//! `gray-usage` sidecar: JSON-lines on stdio. No args (or `sidecar`)
//! runs the wire loop; `report [all]` is the standalone CLI.

use gray_usage as g;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn sidecar() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if req["method"] == "plugin/shutdown" {
            break;
        }
        let Some(id) = req.get("id").cloned() else {
            // event/notify turn_end → live counters (no reply).
            if req["method"] == "event/notify" && req["params"]["type"] == "turn_end" {
                let sid = req["params"]["session"]["id"].as_str().unwrap_or("");
                g::record_turn(sid, &req["params"]["usage"]);
            }
            continue;
        };
        let result = match req["method"].as_str().unwrap_or("") {
            "plugin/manifest" => Ok(g::manifest()),
            "command/run" => {
                let argv: Vec<String> = req["params"]["argv"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                Ok(g::usage_command(&argv))
            }
            _ => Err(format!("unsupported method {}", req["method"])),
        };
        let reply = match result {
            Ok(r) => json!({"id": id, "result": r}),
            Err(e) => json!({"id": id, "error": {"code": -32601, "message": e}}),
        };
        let mut o = stdout.lock();
        let _ = writeln!(o, "{reply}");
        let _ = o.flush();
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("manifest") => println!("{}", g::manifest()),
        Some("report") => print!("{}", g::report(args.get(1).map(|s| s == "all").unwrap_or(false))),
        Some("reset") => {
            let _ = std::fs::remove_dir_all(g::gray_home().join("usage").join("live"));
            println!("usage: live counters cleared");
        }
        _ => {
            if let Err(e) = sidecar() {
                eprintln!("gray-usage: {e}");
            }
        }
    }
}
