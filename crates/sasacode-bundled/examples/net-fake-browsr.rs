//! Stands in for browsr-agent (packages/bundled/test/fake-browsr.ts): `manifest` prints a manifest
//! v1, `serve` runs an MCP server over stdio (one JSON-RPC message a line) with search and open.
//! FAKE_SCHEMA: the tool schema version the manifest reports (1 when unset).
//! Built by `cargo test -p sasacode-bundled`; the net_web tests start it.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn tools() -> Value {
    json!([
        { "name": "search", "description": "Search the web. Returns numbered results.", "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] } },
        { "name": "open", "description": "Open a URL or a number from results/links. Returns page text.", "inputSchema": { "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] } },
    ])
}

fn answer(id: Value, method: &str, params: &Value) -> Value {
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").cloned().unwrap_or_else(|| json!("2025-06-18")),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "fake-browsr", "version": "0.2.0" },
        }),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let body = if name == "search" {
                json!({ "results": [{ "id": 1, "title": "Result", "url": "https://example.com" }] })
            } else {
                let url = params
                    .pointer("/arguments/url")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined");
                json!({ "id": 1, "text": format!("page {url}") })
            };
            json!({ "content": [{ "type": "text", "text": body.to_string() }] })
        }
        "ping" => json!({}),
        _ => {
            return json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Method not found: {method}") } });
        }
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn main() {
    let schema: f64 = std::env::var("FAKE_SCHEMA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    match std::env::args().nth(1).as_deref() {
        Some("manifest") => {
            let manifest = json!({
                "manifest_version": 1, "name": "browsr-4-agent", "version": "0.2.0", "tool_schema_version": schema,
                "default_mode": "standard", "modes": { "standard": tools(), "single": [] },
                "entry": { "command": "browsr-agent", "args": ["serve"] },
            });
            println!("{manifest}");
        }
        Some("serve") => {
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                // Notifications (no id) need no answer.
                let (Some(id), Some(method)) = (msg.get("id").cloned(), msg.get("method").and_then(Value::as_str))
                else {
                    continue;
                };
                let reply = answer(id, method, msg.get("params").unwrap_or(&Value::Null));
                if writeln!(stdout, "{reply}").and_then(|_| stdout.flush()).is_err() {
                    break;
                }
            }
        }
        _ => {}
    }
}
