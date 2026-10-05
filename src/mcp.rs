//! Model Context Protocol server over stdio.
//!
//! Hand-written rather than built on an SDK: the stdio transport is newline
//! delimited JSON-RPC and this server needs four methods, so staying
//! synchronous keeps startup instant and avoids an async runtime.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::tools;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Answers one JSON-RPC message. Notifications produce no reply.
pub fn handle(line: &str) -> Option<Value> {
    // A panic in one call must not take the caller down.
    handle_with(line, &|name, args, _| {
        catch_unwind(AssertUnwindSafe(|| tools::call(name, args)))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error while running {name}")))
    })
}

/// Runs a tool by name with its arguments and the request's progress token, if it sent one.
pub type Runner<'a> = dyn Fn(&str, Value, Option<&Value>) -> anyhow::Result<Value> + 'a;

/// Answers one JSON-RPC message, running tool calls through `run`.
///
/// `run` also receives the progress token of the request, when the caller sent one.
pub fn handle_with(line: &str, run: &Runner<'_>) -> Option<Value> {
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
            let token = params.get("_meta").and_then(|m| m.get("progressToken"));
            let outcome = run(name, args, token);
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
///
/// With `progress`, the call streams, and every event it prints is handed over as it arrives.
fn isolated(
    name: &str,
    args: Value,
    root: Option<&Path>,
    max_memory: usize,
    timeout: u64,
    progress: Option<&dyn Fn(Value)>,
) -> anyhow::Result<Value> {
    let mut command = Command::new(std::env::current_exe()?);
    if progress.is_some() {
        command.arg("--stream");
    }
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
    // Standard error is drained on the side, or a child that fills it would stall.
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    // Every line is one JSON document: progress events first, the result last.
    let mut result = None;
    for line in BufReader::new(child.stdout.take().expect("stdout was piped")).lines() {
        let Ok(value) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        match progress {
            Some(report) if value["event"] == "progress" => report(value),
            _ => result = Some(value),
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if status.success() {
        return result.ok_or_else(|| anyhow::anyhow!("{name} returned nothing"));
    }
    let said: Option<Value> = errors
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l).ok());
    match said.as_ref().and_then(|v| v["error"].as_str()) {
        Some(message) => anyhow::bail!("{message}"),
        None => anyhow::bail!("{name} stopped unexpectedly ({status})"),
    }
}

/// A progress event as the notification MCP defines for it.
pub fn progress_notification(token: &Value, event: &Value) -> Value {
    let mut message = event["step"].as_str().unwrap_or("working").to_string();
    if let Some(page) = event["page"].as_u64() {
        message += &format!(": page {page}");
    }
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {
            "progressToken": token,
            "progress": event["done"],
            "total": event["total"],
            "message": message,
        },
    })
}

/// Serves requests from stdin until it closes, each tool call in its own process.
pub fn serve(root: Option<&Path>, max_memory: usize, timeout: u64) -> std::io::Result<()> {
    // Replies and the progress notifications of a running call share the one output.
    let stdout = RefCell::new(std::io::stdout().lock());
    let say = |message: &Value| -> std::io::Result<()> {
        let mut out = stdout.borrow_mut();
        writeln!(out, "{message}")?;
        out.flush()
    };
    let run = |name: &str, args: Value, token: Option<&Value>| {
        let report = |event: Value| {
            if let Some(token) = token {
                let _ = say(&progress_notification(token, &event));
            }
        };
        let progress: Option<&dyn Fn(Value)> = token.is_some().then_some(&report);
        isolated(name, args, root, max_memory, timeout, progress)
    };
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_with(&line, &run) {
            say(&reply)?;
        }
    }
    Ok(())
}
