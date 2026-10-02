#![cfg(windows)]

// The same process fixture executes against the standalone host crate and
// against managed MCP. This never discovers an installed host.
#[path = "../chrome/native-host/tests/windows_bridge.rs"]
mod native;

use native::{endpoint, hello, result, route, Host, DEADLINE, ID};
use nova_chrome_bridge::AppBridgeListener;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Mcp {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
}

impl Mcp {
    fn spawn(arguments: &[&str], path: &Path, id: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nova"));
        command
            .args(arguments)
            .env("NOVA_CHROME_PIPE", path)
            .env_remove("NOVA_CHROME_EXTENSION_ID");
        if let Some(id) = id {
            command.env("NOVA_CHROME_EXTENSION_ID", id);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = serde_json::from_str(&line.unwrap())
                    .expect("stdout must contain only MCP JSON");
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.input.as_mut().unwrap(), "{message}").unwrap();
    }
    fn response(&self, id: u64) -> Value {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let value = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }
    fn handshake(&mut self) {
        self.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"windows-bridge-test","version":"1"}
        }}));
        assert!(self.response(1)["result"]["serverInfo"].is_object());
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        self.send(json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
        assert_eq!(self.response(2)["result"], json!({}));
    }
    fn chrome(&mut self, id: u64, tool: &str) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":{}}}));
        self.response(id)
    }
    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "managed Nova did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stderr(&mut self) -> String {
        let mut text = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_pipe(path: &Path) {
    let name: Vec<u16> = path
        .to_str()
        .unwrap()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: bounded readiness check on this test's local endpoint; no connection.
    unsafe {
        windows::Win32::System::Pipes::WaitNamedPipeW(windows::core::PCWSTR(name.as_ptr()), 5000)
            .ok()
            .unwrap();
    }
}

fn successful(response: &Value) -> bool {
    response["result"]["isError"] != true && response.get("error").is_none()
}

fn extension(
    path: &Path,
    actions: Vec<&'static str>,
) -> std::thread::JoinHandle<(Host, Vec<Value>)> {
    wait_for_pipe(path);
    let mut host = Host::spawn(path);
    host.send(&hello());
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for action in actions {
            let request = host.receive();
            assert_eq!(request["action"], action);
            let body = match action {
                "status" => json!({"paired":false}),
                "pair" => json!({"route":route()}),
                "read" => {
                    assert_eq!(request["route"], route());
                    json!({"snapshotId":"snapshot-1","nodes":[]})
                }
                _ => unreachable!(),
            };
            host.send(&result(&request, body));
            let receipt = host.receive();
            assert_eq!(receipt["kind"], "receipt");
            assert_eq!(receipt["requestId"], request["requestId"]);
            requests.push(request);
        }
        (host, requests)
    })
}

fn ready_status(mcp: &mut Mcp) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let response = mcp.chrome(10, "chrome_status");
        if successful(&response) {
            return response;
        }
        assert!(Instant::now() < deadline, "{response}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn windows_managed_mcp_pair_read_receipts_second_process_reconnect_and_shutdown() {
    let path = endpoint();
    let mut mcp = Mcp::spawn(&["mcp"], &path, Some(ID));
    mcp.handshake();
    let driver = extension(&path, vec!["status", "pair", "read"]);
    ready_status(&mut mcp);
    assert!(successful(&mcp.chrome(11, "chrome_pair")));
    assert!(successful(&mcp.chrome(12, "chrome_read")));
    let (mut host, requests) = driver.join().unwrap();
    assert_eq!(requests.len(), 3);
    let mut second = Mcp::spawn(&["mcp"], &path, Some(ID));
    assert!(!second.wait().success());
    assert!(second.stderr().contains("another broker"));
    drop(host.input.take());
    assert!(host.wait().success());
    assert!(
        AppBridgeListener::bind(&path).is_err(),
        "broker ownership must survive host loss"
    );
    let driver = extension(&path, vec!["status"]);
    ready_status(&mut mcp);
    let (mut host, _) = driver.join().unwrap();
    drop(mcp.input.take());
    assert!(mcp.wait().success());
    assert!(
        host.wait().success(),
        "broker exit must stop host despite held-open stdin"
    );
    assert!(host.input.is_some());
    assert!(AppBridgeListener::bind(&path).is_ok());
}

#[test]
fn windows_managed_mcp_unanswered_request_revokes_old_host_and_recovers() {
    let path = endpoint();
    let mut mcp = Mcp::spawn(&["mcp"], &path, Some(ID));
    mcp.handshake();
    let driver = extension(&path, vec!["status"]);
    ready_status(&mut mcp);
    let (mut host, _) = driver.join().unwrap();
    mcp.send(json!({"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"chrome_status","arguments":{}}}));
    let abandoned = host.receive();
    assert_eq!(abandoned["action"], "status");
    let started = Instant::now();
    let response = mcp.response(20);
    assert!(!successful(&response));
    assert!(response.to_string().contains("timed out"));
    assert!(started.elapsed() < DEADLINE);
    assert!(host.wait().success());
    assert!(host.input.is_some());
    // The old pipe is closed; a stale result can no longer enter the new session.
    assert!(host
        .input
        .as_mut()
        .unwrap()
        .write_all(
            &nova_chrome_bridge::framing::encode_native(&result(&abandoned, json!({}))).unwrap()
        )
        .is_err());
    let driver = extension(&path, vec!["status"]);
    ready_status(&mut mcp);
    let (mut recovered, requests) = driver.join().unwrap();
    assert_ne!(requests[0]["requestId"], abandoned["requestId"]);
    drop(mcp.input.take());
    assert!(mcp.wait().success());
    assert!(recovered.wait().success());
}

#[test]
fn windows_managed_mcp_invalid_or_incomplete_config_fails_without_owning_endpoint() {
    for configured in [None, Some("bad")] {
        let path = endpoint();
        let mut mcp = Mcp::spawn(&["mcp"], &path, configured);
        assert!(!mcp.wait().success());
        assert!(mcp.stderr().contains("NOVA_CHROME_EXTENSION_ID"));
        assert!(AppBridgeListener::bind(path).is_ok());
    }
}

#[test]
fn windows_bare_stdio_and_http_do_not_acquire_configured_chrome_authority() {
    let path = endpoint();
    let mut bare = Mcp::spawn(&[], &path, Some(ID));
    bare.handshake();
    let unavailable = bare.chrome(10, "chrome_status");
    assert!(!successful(&unavailable));
    assert!(unavailable.to_string().contains("use nova mcp"));
    assert!(AppBridgeListener::bind(&path).is_ok());
    drop(bare.input.take());
    assert!(bare.wait().success());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let _http = Mcp::spawn(&["--http", "--addr", &address], &path, Some(ID));
    let deadline = Instant::now() + DEADLINE;
    while std::net::TcpStream::connect(&address).is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        AppBridgeListener::bind(&path).is_ok(),
        "HTTP must not bind or inject the semantic broker"
    );
}
