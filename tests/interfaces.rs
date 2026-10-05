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
        let no_input = ["pdf_merge", "pdf_ocr_langs", "pdf_ocr_install"].contains(&name);
        assert!(
            tool["inputSchema"]["properties"]["input"].is_object() || no_input,
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

#[test]
fn ocr_install_downloads_language_data_once() {
    let dir = tempfile::tempdir().unwrap();
    let model = vec![7u8; 4096];
    let base = common::serve(vec![
        ("/tst.traineddata", model.clone()),
        ("/tiny.traineddata", vec![1; 10]),
    ]);

    let (file, size, fetched) =
        pdfops::ops::ocr::install_language("tst", dir.path(), &base).unwrap();
    assert!(fetched && size == 4096);
    assert_eq!(std::fs::read(&file).unwrap(), model);
    // Already present: no second download, and nothing half-written left behind.
    let (_, _, fetched) =
        pdfops::ops::ocr::install_language("tst", dir.path(), "http://127.0.0.1:1").unwrap();
    assert!(!fetched);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let err = |lang: &str| {
        format!(
            "{:#}",
            pdfops::ops::ocr::install_language(lang, dir.path(), &base).unwrap_err()
        )
    };
    assert!(
        err("nope").contains("no language 'nope'"),
        "{}",
        err("nope")
    );
    assert!(err("tiny").contains("not a language model"));
    // A code is never allowed to reach outside the data directory or the server path.
    assert!(err("../x").contains("invalid language code"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn cli_ocr_install_then_lists_the_language() {
    let dir = tempfile::tempdir().unwrap();
    let base = common::serve(vec![("/tst.traineddata", vec![7u8; 4096])]);
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_pdfops"))
            .args(args)
            .env("PDFOPS_TESSDATA", dir.path())
            .env("PDFOPS_TESSDATA_URL", &base)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    let (ok, stdout, stderr) = run(&["ocr-install", "--lang", "tst"]);
    assert!(ok, "{stderr}");
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["languages"][0]["downloaded"], true);
    assert!(dir.path().join("tst.traineddata").is_file());

    let (_, stdout, _) = run(&["ocr-langs"]);
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["downloaded_languages"], json!(["tst"]));
    assert_eq!(v["tessdata_dir"], json!(dir.path()));

    let (ok, _, stderr) = run(&["ocr-install"]);
    assert!(!ok && stderr.contains("nothing to install"));
}

#[test]
fn engine_install_command_follows_the_package_manager() {
    use pdfops::ops::ocr::engine_command;
    let (cmd, root) = engine_command(|p| p == "apt-get").unwrap();
    assert_eq!(
        (cmd.join(" ").as_str(), root),
        ("apt-get install -y tesseract-ocr", true)
    );
    let (cmd, root) = engine_command(|p| p == "brew").unwrap();
    assert_eq!(
        (cmd.join(" ").as_str(), root),
        ("brew install tesseract", false)
    );
    assert!(engine_command(|_| false).is_none());
}

/// Sends tool calls to a server confined to `root` and returns each result.
fn confined(root: &std::path::Path, calls: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfops"))
        .args(["mcp", "--root"])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for (i, params) in calls.iter().enumerate() {
            writeln!(
                stdin,
                "{}",
                json!({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": params})
            )
            .unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap()["result"].clone())
        .collect()
}

#[test]
fn mcp_root_confines_every_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    std::fs::create_dir(&root).unwrap();
    common::sample(&root, "in.pdf", 2);
    let outside = common::sample(dir.path(), "secret.pdf", 1);
    std::fs::write(
        root.join("notes.md"),
        format!("![x]({})", dir.path().join("pic.png").display()),
    )
    .unwrap();

    let results = confined(
        &root,
        &[
            // Inside, by relative path: resolved against the root, not the server's start directory.
            json!({"name": "pdf_info", "arguments": {"input": "in.pdf"}}),
            json!({"name": "pdf_rotate", "arguments": {"input": "in.pdf", "output": "sub/out.pdf", "angle": 90}}),
            // Outside: reading, writing, by absolute path and by climbing.
            json!({"name": "pdf_info", "arguments": {"input": outside}}),
            json!({"name": "pdf_info", "arguments": {"input": "../secret.pdf"}}),
            json!({"name": "pdf_rotate", "arguments": {"input": "in.pdf", "output": "../out.pdf", "angle": 90}}),
            json!({"name": "pdf_merge", "arguments": {"inputs": ["in.pdf", outside], "output": "m.pdf"}}),
            json!({"name": "pdf_split", "arguments": {"input": "in.pdf", "out_dir": dir.path().join("parts")}}),
            // A path that arrives inside a document rather than as an argument.
            json!({"name": "pdf_create", "arguments": {"input": "notes.md", "output": "notes.pdf"}}),
            json!({"name": "pdf_ocr_install", "arguments": {"engine": true}}),
        ],
    );
    assert_eq!(results.len(), 9);
    assert_eq!(results[0]["isError"], false, "{}", results[0]);
    assert_eq!(results[0]["structuredContent"]["pages"], 2);
    assert_eq!(results[1]["isError"], false, "{}", results[1]);
    assert!(root.join("sub/out.pdf").is_file());
    for (i, result) in results.iter().enumerate().skip(2) {
        assert_eq!(
            result["isError"], true,
            "call {i} should be refused: {result}"
        );
    }
    for result in &results[2..8] {
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("outside the allowed directory"),
            "{result}"
        );
    }
    assert!(
        !dir.path().join("out.pdf").exists()
            && !dir.path().join("parts").exists()
            && !root.join("m.pdf").exists()
    );
}
