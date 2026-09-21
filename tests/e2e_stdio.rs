//! Regression test for the stdio transport.
//!
//! The server must stay alive through the MCP handshake and keep answering
//! requests — not exit immediately after `initialize`. That earlier bug
//! (dropping the `RunningService` handle) manifested to clients as
//! "MCP server stdout closed (EOF) / Server disconnected" right after init.
//!
//! Spawns the real binary; needs no Screen Recording permission (handshake +
//! tools/list only), so it runs in CI.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn stdio_server_keeps_connection_after_bamboo_server_discover() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nova binary");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let client_meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    });

    let send = |stdin: &mut std::process::ChildStdin, message: Value| {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    };
    let response = |stdout: &mut BufReader<_>, id: u64| -> Value {
        let mut line = String::new();
        loop {
            line.clear();
            stdout.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "Nova stdout closed before response {id}");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value.get("id") == Some(&json!(id)) {
                return value;
            }
        }
    };

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 90,
            "method": "server/discover",
            "params": {"_meta": client_meta},
        }),
    );
    let discovery = response(&mut stdout, 90);
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert!(discovery["result"]["supportedVersions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|version| version == "2026-07-28"));

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "bamboo-discover-regression", "version": "1"},
            },
        }),
    );
    let initialized = response(&mut stdout, 1);
    assert!(initialized["result"]["serverInfo"].is_object());
    assert_eq!(initialized["result"]["protocolVersion"], "2024-11-05");
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {"_meta": client_meta},
        }),
    );
    let tools = response(&mut stdout, 2);
    assert!(tools["result"]["tools"].as_array().unwrap().len() >= 16);

    // tools/list is deliberately the post-initialize liveness probe: Nova's
    // handler does not expose the optional ping method.
    assert!(
        child.try_wait().unwrap().is_none(),
        "discover closed the connection"
    );

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "server exited unsuccessfully: {status}");
}

#[test]
fn stdio_server_completes_handshake_and_lists_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nova binary");

    let mut stdin = child.stdin.take().unwrap();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let inited = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    write!(stdin, "{init}\n{inited}\n{list}\n").unwrap();
    // EOF on stdin should trigger a clean shutdown *after* both requests are
    // answered — if the server died after init, id:2 never gets a response.
    drop(stdin);

    // Read stdout to EOF on a worker thread with a watchdog so a hung/never-
    // exiting server fails the test instead of blocking forever.
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });

    let out = match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(out) => out,
        Err(_) => {
            let _ = child.kill();
            panic!("stdio server did not shut down on stdin EOF within 15s");
        }
    };

    let status = child.wait().expect("wait for nova");
    assert!(status.success(), "server exited unsuccessfully: {status}");

    assert!(
        out.contains("\"serverInfo\""),
        "missing initialize response:\n{out}"
    );
    assert!(
        out.contains("\"id\":2"),
        "no tools/list response — server died after initialize:\n{out}"
    );
    let tool_count = out.matches("\"name\":").count();
    assert!(
        tool_count >= 16,
        "expected >=16 tools listed, got {tool_count}:\n{out}"
    );
}
