//! `6ixpack mcp`: a stdio MCP server (JSON-RPC 2.0, one message per line).
//! Each tool runs this same executable as a child process with the given
//! arguments, so command output never mixes with the protocol stream.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::Command;

const MAX_OUT: usize = 200_000;

const TOOLS: &[(&str, &str, &str)] = &[
    ("extract", "", "Scan an RPF9 archive and extract its payloads: args like [\"-n\", \"levels.rpf\"] or [\"levels.rpf\", \"outdir\"]. Pass [\"--help\"] for options."),
    ("audio", "audio", "AWC/ADAT audio: list banks, extract Opus, Ogg or WAV. Pass [\"--help\"] for options."),
    ("texture", "texture", "RAGE textures and TTPF stores to DDS/PNG. Pass [\"--help\"] for options."),
    ("model", "model", "RAGE drawables to OBJ. Pass [\"--help\"] for options."),
    ("convert", "convert", "Convert to GTA V formats: ytd (DDS dir), ydr/ybn (OBJ), ymap/ytyp (JSON). Pass [] for usage."),
    ("sixng", "sixng", "SixNG commands: placements, vehicles, GLB export, ttpf, names, interiors, movies, ... Pass [\"--help\"] (or [\"<command>\", \"--help\"]) for the list."),
];

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            send(&mut out, json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}))?;
            continue;
        };
        let Some(id) = msg.get("id").cloned() else { continue }; // notification
        let method = msg["method"].as_str().unwrap_or("");
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "6ixpack", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": TOOLS.iter().map(|(name, _, desc)| json!({
                "name": name,
                "description": desc,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "args": {"type": "array", "items": {"type": "string"}, "description": "Command-line arguments"},
                        "threads": {"type": "integer", "description": "Worker threads (default: all cores)"}
                    },
                    "required": ["args"]
                }
            })).collect::<Vec<_>>()})),
            "tools/call" => call(&msg["params"]),
            _ => Err((-32601, format!("unknown method {method}"))),
        };
        let resp = match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        send(&mut out, resp)?;
    }
    Ok(())
}

fn send(out: &mut impl Write, v: Value) -> std::io::Result<()> {
    writeln!(out, "{v}")?;
    out.flush()
}

fn call(params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or("");
    let (_, sub, _) = TOOLS.iter().find(|t| t.0 == name).ok_or((-32602, format!("unknown tool {name}")))?;
    let args: Vec<String> = params["arguments"]["args"]
        .as_array()
        .ok_or((-32602, "args must be an array of strings".to_string()))?
        .iter()
        .map(|a| a.as_str().map(str::to_string).ok_or((-32602, "args must be strings".to_string())))
        .collect::<Result<_, _>>()?;
    let exe = std::env::current_exe().map_err(|e| (-32603, e.to_string()))?;
    let mut cmd = Command::new(exe);
    if let Some(n) = params["arguments"]["threads"].as_u64() {
        cmd.arg("--threads").arg(n.to_string());
    }
    if !sub.is_empty() {
        cmd.arg(sub);
    }
    let o = cmd.args(&args).stdin(std::process::Stdio::null()).output().map_err(|e| (-32603, e.to_string()))?;
    let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
    let err = String::from_utf8_lossy(&o.stderr);
    if !err.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&err);
    }
    if text.len() > MAX_OUT {
        let cut = (0..=MAX_OUT).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
        text.truncate(cut);
        text.push_str("\n[output truncated]");
    }
    Ok(json!({"content": [{"type": "text", "text": text}], "isError": !o.status.success()}))
}
