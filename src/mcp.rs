//! Model Context Protocol server over stdio.
//!
//! Hand-written rather than built on an SDK: the stdio transport is newline
//! delimited JSON-RPC and this server needs four methods, so staying
//! synchronous keeps startup instant and avoids an async runtime.

use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::tools;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Answers one JSON-RPC message. Notifications produce no reply.
pub fn handle(line: &str) -> Option<Value> {
    // A panic in one call must not take the caller down.
    handle_with(line, &|name, args| {
        catch_unwind(AssertUnwindSafe(|| tools::call(name, args)))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error while running {name}")))
    })
}

/// Answers one JSON-RPC message, running tool calls through `run`.
pub fn handle_with(
    line: &str,
    run: &dyn Fn(&str, Value) -> anyhow::Result<Value>,
) -> Option<Value> {
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
            let outcome = run(name, args);
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

/// Runs a tool call in a process of its own.
///
/// A file that exhausts memory or never finishes then costs one failed call
/// instead of the server, and the limits are enforced by the operating system
/// boundary rather than by good behaviour.
fn isolated(
    name: &str,
    args: Value,
    root: Option<&Path>,
    max_memory: usize,
    timeout: u64,
) -> anyhow::Result<Value> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--max-memory",
            &max_memory.to_string(),
            "--timeout",
            &timeout.to_string(),
            "call",
            name,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root) = root {
        command.arg("--root").arg(root);
    }
    let mut child = command.spawn()?;
    // The arguments are small; the child reads them all before it writes anything.
    serde_json::to_writer(child.stdin.take().expect("stdin was piped"), &args)?;
    let out = child.wait_with_output()?;
    if out.status.success() {
        return Ok(serde_json::from_slice(&out.stdout)?);
    }
    let said: Option<Value> = String::from_utf8_lossy(&out.stderr)
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok());
    match said.as_ref().and_then(|v| v["error"].as_str()) {
        Some(message) => anyhow::bail!("{message}"),
        None => anyhow::bail!("{name} stopped unexpectedly ({})", out.status),
    }
}

/// Serves requests from stdin until it closes, each tool call in its own process.
pub fn serve(root: Option<&Path>, max_memory: usize, timeout: u64) -> std::io::Result<()> {
    let run = |name: &str, args: Value| isolated(name, args, root, max_memory, timeout);
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_with(&line, &run) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}
