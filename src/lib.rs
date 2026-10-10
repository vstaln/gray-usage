//! gray-usage — port of pi-usage + pi-harness-runtime /usage.
//!
//! `/usage` reports per-session token accounting by scanning
//! `~/.gray/sessions/*.jsonl` (header line carries id/model/cwd; every
//! entry can carry a `usage` object), costs priced by a small
//! configurable rate table, plus provider balance endpoints where they
//! exist (config map provider→endpoint; shipped defaults cover
//! openrouter/opencode-zen/xai and degrade to "unreachable" rows).
//! `turn_end` usage is accumulated into `~/.gray/usage/live/<sid>.json`
//! for the live-session row.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

pub const PLUGIN_NAME: &str = "usage";
pub const PROTOCOL: &str = "1.1";
const DEFAULT_LIMIT: usize = 25;

pub fn gray_home() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAY_HOME") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".gray")
}

fn cfg_path() -> PathBuf {
    gray_home().join("usage").join("config.json")
}

// ---------- rates (USD per million tokens; configurable, approximate) ----------

#[derive(Debug, Clone, Copy)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

/// Built-in estimates; `~/.gray/usage/config.json` `rates` overrides or
/// extends (substring match on the model id). Prices drift — treat the
/// column as an estimate.
fn builtin_rates() -> Vec<(&'static str, Rate)> {
    vec![
        ("claude-opus", Rate { input: 15.0, output: 75.0, cache_read: 1.50 }),
        ("claude-sonnet", Rate { input: 3.0, output: 15.0, cache_read: 0.30 }),
        ("claude-haiku", Rate { input: 0.80, output: 4.0, cache_read: 0.08 }),
        ("gpt-5", Rate { input: 1.25, output: 10.0, cache_read: 0.125 }),
        ("grok-4", Rate { input: 3.0, output: 15.0, cache_read: 0.75 }),
        ("grok-code", Rate { input: 1.50, output: 15.0, cache_read: 0.15 }),
    ]
}

fn config_rates() -> Vec<(String, Rate)> {
    let Ok(raw) = std::fs::read_to_string(cfg_path()) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    v["rates"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, r)| {
                    Some((
                        k.clone(),
                        Rate {
                            input: r["input"].as_f64()?,
                            output: r["output"].as_f64()?,
                            cache_read: r["cache_read"].as_f64().unwrap_or(0.0),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn rate_for(model: &str) -> Option<Rate> {
    for (k, r) in config_rates() {
        if model.contains(&k) {
            return Some(r);
        }
    }
    builtin_rates()
        .into_iter()
        .find(|(k, _)| model.contains(k))
        .map(|(_, r)| r)
}

fn cost(model: &str, input: u64, output: u64, cache_read: u64) -> Option<f64> {
    let r = rate_for(model)?;
    let fresh = input.saturating_sub(cache_read) as f64;
    Some((fresh * r.input + cache_read as f64 * r.cache_read + output as f64 * r.output) / 1e6)
}

// ---------- session store ----------

#[derive(Debug, Default)]
pub struct SessionRow {
    pub id: String,
    pub model: String,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub ts: u64,
}

/// Parse one session file: header line + per-entry `usage` sums.
fn scan_file(path: &std::path::Path) -> Option<SessionRow> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut lines = raw.lines();
    let head: Value = serde_json::from_str(lines.next()?).ok()?;
    let mut row = SessionRow {
        id: head["id"].as_str().unwrap_or("?").to_string(),
        model: head["model"].as_str().unwrap_or("?").to_string(),
        ts: head["timestamp"].as_u64().unwrap_or(0),
        ..Default::default()
    };
    for line in lines {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let u = &v["usage"];
        if u.is_null() {
            continue;
        }
        let g = |k: &str| u[k].as_u64().unwrap_or(0);
        row.turns += 1;
        row.input += g("input_tokens");
        row.output += g("output_tokens");
        row.cache_read += g("cache_read_input_tokens");
    }
    Some(row)
}

/// Sessions sorted newest-first; `limit` caps the scan (None = all).
pub fn scan_sessions(limit: Option<usize>) -> (Vec<SessionRow>, usize) {
    let dir = gray_home().join("sessions");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|t| std::time::SystemTime::now().duration_since(t).unwrap_or_default())
            .unwrap_or_default()
    });
    let total = files.len();
    let take = limit.unwrap_or(total);
    let rows = files
        .iter()
        .take(take)
        .filter_map(|p| scan_file(p))
        .collect();
    (rows, total)
}

// ---------- live turn_end accumulation ----------

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Live {
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

fn live_path(sid: &str) -> PathBuf {
    let safe: String = sid
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    gray_home().join("usage").join("live").join(format!("{safe}.json"))
}

/// Fold one `turn_end` usage into `~/.gray/usage/live/<sid>.json`.
pub fn record_turn(sid: &str, usage: &Value) {
    if sid.is_empty() {
        return;
    }
    let p = live_path(sid);
    let mut l: Live = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let g = |k: &str| usage[k].as_u64().unwrap_or(0);
    l.turns += 1;
    l.input += g("input_tokens");
    l.output += g("output_tokens");
    l.cache_read += g("cache_read_input_tokens");
    l.cache_write += g("cache_write_input_tokens");
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, serde_json::to_string(&l).unwrap_or_default());
}

fn live_rows() -> Vec<(String, Live)> {
    let dir = gray_home().join("usage").join("live");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(l) = std::fs::read_to_string(&p)
                .and_then(|s| serde_json::from_str::<Live>(&s).map_err(|e| e.into()))
            {
                let sid = p.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                out.push((sid, l));
            }
        }
    }
    out.sort_by(|a, b| b.1.turns.cmp(&a.1.turns));
    out
}

// ---------- provider balances ----------

#[derive(Debug, Deserialize)]
struct Balance {
    url: String,
    #[serde(default)]
    auth_env: String,
    #[serde(default)]
    auth_key: String,
}

/// provider→endpoint map: config `providers` overrides the shipped
/// defaults. `auth_key` names an entry in `~/.gray/auth.json`;
/// `auth_env` names an env var holding a bearer token.
fn balance_map() -> Vec<(String, Balance)> {
    let mut map: Vec<(String, Balance)> = serde_json::from_value::<
        std::collections::BTreeMap<String, Balance>,
    >(json!({
        "openrouter":   {"url": "https://openrouter.ai/api/v1/credits", "auth_key": "openrouter"},
        "opencode-zen": {"url": "https://opencode.ai/zen/v1/balance",   "auth_key": "opencode-go"},
        "xai":          {"url": "https://api.x.ai/v1/billing/balance",  "auth_env": "XAI_API_KEY"},
    }))
    .unwrap_or_default()
    .into_iter()
    .collect();
    if let Ok(raw) = std::fs::read_to_string(cfg_path()) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(m) = v["providers"].as_object() {
                for (k, b) in m {
                    if let Ok(b) = serde_json::from_value::<Balance>(b.clone()) {
                        map.retain(|(n, _)| n != k);
                        map.push((k.clone(), b));
                    }
                }
            }
        }
    }
    map
}

fn auth_token(b: &Balance) -> Option<String> {
    if !b.auth_env.is_empty() {
        if let Ok(t) = std::env::var(&b.auth_env) {
            if !t.trim().is_empty() {
                return Some(t);
            }
        }
    }
    if !b.auth_key.is_empty() {
        let raw = std::fs::read_to_string(gray_home().join("auth.json")).ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        return v[&b.auth_key].as_str().map(str::to_string);
    }
    None
}

/// Curl the endpoint (network stays a subprocess per the porting guide)
/// and surface any balance-shaped fields from the reply.
fn fetch_balance(name: &str, b: &Balance) -> String {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-m", "8", "-o", "-", "-w", "\n%{http_code}", &b.url]);
    if let Some(t) = auth_token(b) {
        cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => return format!("{name:<14} no curl ({e})"),
    };
    let body = String::from_utf8_lossy(&out.stdout);
    let (json_part, code) = body.rsplit_once('\n').unwrap_or((&body, ""));
    if code != "200" {
        return format!("{name:<14} unreachable (HTTP {code})");
    }
    let Ok(v) = serde_json::from_str::<Value>(json_part) else {
        return format!("{name:<14} non-JSON reply");
    };
    let mut fields = Vec::new();
    collect_balance_fields(&v, "", &mut fields);
    if fields.is_empty() {
        format!("{name:<14} ok (no balance fields in reply)")
    } else {
        format!("{name:<14} {}", fields.join("  "))
    }
}

fn collect_balance_fields(v: &Value, path: &str, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                collect_balance_fields(x, &p, out);
            }
        }
        Value::Number(n) => {
            let leaf = path.rsplit('.').next().unwrap_or(path).to_lowercase();
            if out.len() < 6
                && ["balance", "credit", "credits", "remaining", "amount", "usage"]
                    .iter()
                    .any(|w| leaf.contains(w))
            {
                out.push(format!("{path}={n}"));
            }
        }
        _ => {}
    }
}

// ---------- report ----------

fn fmt_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn report(all: bool) -> String {
    let (rows, total_files) = scan_sessions(if all { None } else { Some(DEFAULT_LIMIT) });
    let mut out = format!("usage — {} of {} sessions\n", rows.len(), total_files);
    out.push_str(&format!(
        "{:<10} {:<18} {:>5} {:>8} {:>8} {:>8} {:>8}\n",
        "session", "model", "turns", "input", "output", "cached", "est$"
    ));
    let (mut ti, mut to, mut tc, mut cost_sum, mut priced) = (0u64, 0u64, 0u64, 0.0, false);
    for r in &rows {
        let c = cost(&r.model, r.input, r.output, r.cache_read);
        let cs = c.map(|x| format!("{x:>8.2}")).unwrap_or_else(|| "       -".to_string());
        if let Some(x) = c {
            cost_sum += x;
            priced = true;
        }
        ti += r.input;
        to += r.output;
        tc += r.cache_read;
        let id: String = r.id.chars().take(10).collect();
        let model: String = r.model.chars().take(18).collect();
        out.push_str(&format!(
            "{id:<10} {model:<18} {:>5} {:>8} {:>8} {:>8} {cs}\n",
            r.turns,
            fmt_num(r.input),
            fmt_num(r.output),
            fmt_num(r.cache_read),
        ));
    }
    out.push_str(&format!(
        "{:<10} {:<18} {:>5} {:>8} {:>8} {:>8} {:>8}\n",
        "TOTAL",
        "",
        rows.iter().map(|r| r.turns).sum::<u64>(),
        fmt_num(ti),
        fmt_num(to),
        fmt_num(tc),
        if priced { format!("{cost_sum:.2}") } else { "-".into() },
    ));
    let live = live_rows();
    if !live.is_empty() {
        out.push_str("\nlive (turn_end this run):\n");
        for (sid, l) in live.iter().take(5) {
            let short: String = sid.chars().take(16).collect();
            out.push_str(&format!(
                "  {short:<16} turns={} in={} out={} cached={}\n",
                l.turns,
                fmt_num(l.input),
                fmt_num(l.output),
                fmt_num(l.cache_read),
            ));
        }
    }
    out.push_str("\nprovider balances:\n");
    // parallel: each curl caps at 8s; sequential could exceed the host's
    // command/run TTL when endpoints hang instead of refusing.
    let handles: Vec<_> = balance_map()
        .into_iter()
        .map(|(name, b)| std::thread::spawn(move || fetch_balance(&name, &b)))
        .collect();
    for h in handles {
        if let Ok(line) = h.join() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

/// `command/run` handler for `/usage` and `/tokens`.
pub fn usage_command(argv: &[String]) -> Value {
    match argv.first().map(|s| s.as_str()) {
        Some("all") | None => json!({"text": report(argv.first().is_some())}),
        Some("reset") => {
            let _ = std::fs::remove_dir_all(gray_home().join("usage").join("live"));
            json!({"text": "usage: live counters cleared"})
        }
        Some(_) => json!({"text": "usage: /usage [all|reset] — per-session tokens+cost, live counters, provider balances"}),
    }
}

pub fn manifest() -> Value {
    json!({
        "name": PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL,
        "sidecar_args": ["sidecar"],
        "tools": [],
        // /usage is shadowed by gray's builtin /usage in the REPL; /tokens
        // is the unshadowed interactive alias to the same report.
        "commands": ["/usage", "/tokens"],
        "completion": ["all", "reset"],
        "hooks": [],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("usage-test-{}-{tag}", std::process::id()));
        let sess = dir.join("sessions");
        std::fs::create_dir_all(&sess).unwrap();
        let mut f = std::fs::File::create(sess.join("s1.jsonl")).unwrap();
        writeln!(f, "{{\"version\":1,\"id\":\"s1\",\"model\":\"claude-sonnet-4\",\"timestamp\":5}}").unwrap();
        writeln!(f, "{{\"entry_id\":0,\"message\":{{\"role\":\"assistant\"}},\"usage\":{{\"input_tokens\":100,\"output_tokens\":10,\"cache_read_input_tokens\":60}}}}").unwrap();
        writeln!(f, "{{\"entry_id\":1,\"message\":{{\"role\":\"assistant\"}},\"usage\":{{\"input_tokens\":200,\"output_tokens\":20,\"cache_read_input_tokens\":150}}}}").unwrap();
        std::env::set_var("GRAY_HOME", &dir);
        dir
    }

    #[test]
    fn scans_sessions_and_costs() {
        let dir = fixture_home("scan");
        let (rows, total) = scan_sessions(None);
        assert_eq!(total, 1);
        assert_eq!(rows[0].turns, 2);
        assert_eq!(rows[0].input, 300);
        assert_eq!(rows[0].cache_read, 210);
        assert!(cost(&rows[0].model, 300, 20, 210).unwrap() > 0.0);
        assert!(report(false).contains("claude-sonnet-4"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn turn_end_accumulates_live() {
        let dir = fixture_home("live");
        record_turn("sid-1", &json!({"input_tokens": 50, "output_tokens": 5,
            "cache_read_input_tokens": 30, "cache_write_input_tokens": 4}));
        record_turn("sid-1", &json!({"input_tokens": 50, "output_tokens": 5}));
        let rows = live_rows();
        assert_eq!(rows[0].1.turns, 2);
        assert_eq!(rows[0].1.input, 100);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unpriced_model_dashes() {
        assert!(cost("weird-model", 100, 10, 0).is_none());
    }

    #[test]
    fn manifest_claims_usage_alias() {
        let cmds = manifest()["commands"].as_array().unwrap().clone();
        assert!(cmds.contains(&json!("/usage")) && cmds.contains(&json!("/tokens")));
    }
}
