//! Model Context Protocol server over stdio.
//!
//! Hand-written rather than built on an SDK: the stdio transport is newline
//! delimited JSON-RPC and this server needs four methods, so staying
//! synchronous keeps startup instant and avoids an async runtime.

use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::{Value, json};

use crate::tools;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Answers one JSON-RPC message. Notifications produce no reply.
pub fn handle(line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = msg.get("id").cloned()?;
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return Some(error(id, -32600, "missing method"));
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "pdfops", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "PDF tools. Paths are resolved against the server's working directory. Pages are 1-based; page specs look like \"1-3,7,10-\".",
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools::definitions()}),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(error(id, -32602, "missing tool name"));
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            // A panic in one call must not take the server down.
            let outcome = catch_unwind(AssertUnwindSafe(|| tools::call(name, args)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error while running {name}")));
            match outcome {
                Ok(value) => json!({
                    "content": [{"type": "text", "text": value.to_string()}],
                    "structuredContent": value,
                    "isError": false,
                }),
                Err(e) => {
                    json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})
                }
            }
        }
        _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Serves requests from stdin until it closes.
pub fn serve() -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(&line) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}
