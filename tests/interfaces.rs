mod common;

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn rpc(method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    pdfops::mcp::handle(&line).expect("a reply")
}

#[test]
fn mcp_handshake_and_tool_listing() {
    let v = rpc(
        "initialize",
        json!({"protocolVersion": "2025-03-26", "capabilities": {}}),
    );
    assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(v["result"]["serverInfo"]["name"], "pdfops");
    assert!(v["result"]["capabilities"]["tools"].is_object());

    let tools = rpc("tools/list", json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), pdfops::tools::TOOLS.len());
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        assert!(name.starts_with("pdf_") && !name.contains('-'), "{name}");
        // Every registered tool must have a CLI subcommand to take its description from.
        assert!(
            tool["description"].as_str().unwrap().len() > 10,
            "{name} has no description"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert!(
            tool["inputSchema"]["properties"]["input"].is_object() || name == "pdf_merge",
            "{name}"
        );
    }
    let fill = tools.iter().find(|t| t["name"] == "pdf_fill").unwrap();
    assert!(fill["inputSchema"]["properties"]["values"].is_object());
    assert!(fill["inputSchema"]["properties"].get("set").is_none());
}

#[test]
fn mcp_calls_tools_and_reports_failures() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = common::sample(dir.path(), "a.pdf", 2);
    let v = rpc(
        "tools/call",
        json!({"name": "pdf_info", "arguments": {"input": pdf}}),
    );
    assert_eq!(v["result"]["isError"], false);
    assert_eq!(v["result"]["structuredContent"]["pages"], 2);
    let text: Value =
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["pages"], 2);

    let v = rpc(
        "tools/call",
        json!({"name": "pdf_info", "arguments": {"input": dir.path().join("nope.pdf")}}),
    );
    assert_eq!(v["result"]["isError"], true);
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cannot read")
    );
}

#[test]
fn mcp_protocol_edges() {
    assert!(
        pdfops::mcp::handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none()
    );
    assert_eq!(rpc("ping", json!({}))["result"], json!({}));
    assert_eq!(rpc("resources/list", json!({}))["error"]["code"], -32601);
    assert_eq!(
        pdfops::mcp::handle("{oops").unwrap()["error"]["code"],
        -32700
    );
    assert_eq!(rpc("tools/call", json!({}))["error"]["code"], -32602);
}

#[test]
fn cli_prints_json_and_sets_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = common::sample(dir.path(), "a.pdf", 3);
    let bin = env!("CARGO_BIN_EXE_pdfops");

    let out = Command::new(bin).args(["info"]).arg(&pdf).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["pages"], 3);

    let out = Command::new(bin)
        .args(["text", "--raw", "-p", "2"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Page 2 of the sample"));

    let rotated = dir.path().join("r.pdf");
    let out = Command::new(bin)
        .args(["rotate", "--angle", "-90", "-o"])
        .arg(&rotated)
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = Command::new(bin)
        .args(["info"])
        .arg(dir.path().join("nope.pdf"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let v: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert!(v["error"].as_str().unwrap().contains("cannot read"));
}

#[test]
fn cli_fill_takes_repeated_assignments() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = common::form(dir.path());
    let out_path = dir.path().join("f.pdf");
    let out = Command::new(env!("CARGO_BIN_EXE_pdfops"))
        .args(["fill", "--set", "name=A=B", "--set", "agree=yes", "-o"])
        .arg(&out_path)
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fields = common::call("pdf_forms", json!({"input": out_path}));
    assert_eq!(fields["fields"][0]["value"], "A=B");
    assert_eq!(fields["fields"][1]["value"], "Yes");
}

#[test]
fn mcp_server_speaks_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfops"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(
        lines[1]["result"]["tools"].as_array().unwrap().len(),
        pdfops::tools::TOOLS.len()
    );
}
