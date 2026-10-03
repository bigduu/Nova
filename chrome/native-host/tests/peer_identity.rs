#![cfg(any(target_os = "macos", target_os = "linux", windows))]

use nova_chrome_bridge::framing::{encode_native, read_native};
use nova_chrome_bridge::ChromeBridge;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

const ID: &str = "abcdefghijklmnopabcdefghijklmnop";
const LIMIT: Duration = Duration::from_secs(5);

struct Endpoint(PathBuf);
impl Endpoint {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        #[cfg(windows)]
        {
            Self(PathBuf::from(format!(
                r"\\.\pipe\nova-chrome-identity-{suffix}"
            )))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let directory = std::env::temp_dir().join(format!("nova-peer-identity-{suffix}"));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(directory.join("chrome.sock"))
        }
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Broker cleanup is on its existing background thread. Remove only
            // our now-empty directory after that thread unlinks its own socket.
            let deadline = Instant::now() + LIMIT;
            while std::fs::remove_dir(self.0.parent().unwrap()).is_err() {
                assert!(
                    Instant::now() < deadline,
                    "broker did not close its owned endpoint"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

struct Host {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
}
impl Host {
    fn spawn(path: &Endpoint, via_cmd: bool) -> Self {
        let binary = std::env::var_os("NOVA_TEST_CHROME_HOST")
            .map(PathBuf::from)
            .expect("explicitly build/provide NOVA_TEST_CHROME_HOST; fixtures must not skip");
        assert!(binary.is_file(), "required native-host binary is missing");
        let origin = format!("chrome-extension://{ID}/");
        #[cfg(windows)]
        let mut command = if via_cmd {
            use std::os::windows::process::CommandExt;
            use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
            let mut directory = [0u16; 32768];
            // SAFETY: writable directory buffer; returned length is checked.
            let length = unsafe { GetSystemDirectoryW(Some(&mut directory)) } as usize;
            assert!(length > 0 && length < directory.len());
            let cmd =
                PathBuf::from(String::from_utf16(&directory[..length]).unwrap()).join("cmd.exe");
            let mut command = Command::new(cmd);
            command
                .args(["/d", "/s", "/c"])
                .raw_arg(format!("\"\"{}\" \"{origin}\"\"", binary.display()));
            command
        } else {
            let mut command = Command::new(&binary);
            command.arg(&origin);
            command
        };
        #[cfg(unix)]
        let mut command = {
            assert!(!via_cmd);
            let mut command = Command::new(&binary);
            command.arg(origin);
            command
        };
        command.env("NOVA_CHROME_EXTENSION_ID", ID);
        #[cfg(windows)]
        command.env("NOVA_CHROME_PIPE", &path.0);
        #[cfg(unix)]
        command.env("NOVA_CHROME_SOCKET", &path.0);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(message) =
                read_native(&mut stdout).expect("only native frames on stdout")
            {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        let mut host = Self {
            child,
            input,
            output,
        };
        host.send(
            &json!({"protocolVersion":1,"kind":"hello","role":"chrome_extension","extensionId":ID}),
        );
        host
    }
    fn send(&mut self, value: &Value) {
        self.input
            .as_mut()
            .unwrap()
            .write_all(&encode_native(value).unwrap())
            .unwrap();
    }
    fn receive(&self) -> Value {
        self.output.recv_timeout(LIMIT).unwrap()
    }
    fn close(&mut self) {
        drop(self.input.take());
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned host/launcher failed to close"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn worker(mut host: Host, actions: &[&str]) -> std::thread::JoinHandle<Host> {
    let actions: Vec<_> = actions.iter().map(|action| action.to_string()).collect();
    std::thread::spawn(move || {
        for action in actions {
            let request = host.receive();
            assert_eq!(request["action"], action);
            let body = match action.as_str() {
                "pair" => {
                    json!({"route":{"tabId":7,"documentId":"document-7","nonce":"page-7","epoch":3}})
                }
                "release" => json!({"released":true}),
                "status" => {
                    json!({"paired":false,"ownership":{"status":"verified_chrome","kernelPeerPid":1}})
                }
                _ => unreachable!(),
            };
            let mut response = json!({"protocolVersion":1,"kind":"result","requestId":request["requestId"],
                "action":action,"status":"ok","epoch":3,"receipt":{"receiptId":format!("receipt-{}",request["requestId"].as_str().unwrap()),"expiresAt":10000},"result":body});
            if request.get("route").is_some() {
                response["route"] = request["route"].clone();
            }
            host.send(&response);
            let receipt = host.receive();
            assert_eq!(receipt["kind"], "receipt");
            assert_eq!(receipt["requestId"], request["requestId"]);
        }
        host
    })
}

fn ready(bridge: &ChromeBridge) -> Value {
    let deadline = Instant::now() + LIMIT;
    loop {
        match bridge.status() {
            Ok(value) => return value["result"]["ownership"].clone(),
            Err(error) => {
                assert!(Instant::now() < deadline, "{error:#}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn paired_host(endpoint: &Endpoint, bridge: &ChromeBridge) -> Host {
    let driver = worker(Host::spawn(endpoint, false), &["status", "pair"]);
    ready(bridge);
    bridge.pair().unwrap();
    driver.join().unwrap()
}

fn terminal_reply(
    bridge: &ChromeBridge,
    host: &mut Host,
    action: &str,
    mutate: impl FnOnce(&mut Value),
) -> (anyhow::Result<Value>, Value) {
    let caller = bridge.clone();
    let action = action.to_string();
    let requested_action = action.clone();
    let caller = std::thread::spawn(move || caller.call(&action, json!({}), Some(LIMIT)));
    let request = host.receive();
    assert_eq!(request["action"], requested_action);
    let mut result = json!({"protocolVersion":1,"kind":"result",
        "requestId":request["requestId"],"action":requested_action,"status":"ok","epoch":3,
        "receipt":{"receiptId":"terminal-receipt","expiresAt":10000},"result":{}});
    if let Some(route) = request.get("route") {
        result["route"] = route.clone();
    }
    mutate(&mut result);
    host.send(&result);
    assert_eq!(
        host.receive(),
        json!({"protocolVersion":1,"kind":"receipt",
        "receiptId":result["receipt"]["receiptId"],"requestId":result["requestId"],
        "action":result["action"],"epoch":result["epoch"]})
    );
    (caller.join().unwrap(), result)
}

fn assert_disconnected(bridge: &ChromeBridge, host: &mut Host) {
    assert!(matches!(
        host.output.recv_timeout(LIMIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    let error = bridge.read(None, None).unwrap_err().to_string();
    assert!(error.contains("not connected"), "{error}");
    host.close();
}

#[test]
fn matched_status_error_strips_forged_ownership_and_keeps_the_paired_route() {
    let endpoint = Endpoint::new();
    let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
    let mut host = paired_host(&endpoint, &bridge);
    let (delivered, mut expected) = terminal_reply(&bridge, &mut host, "status", |message| {
        message["status"] = json!("error");
        message["error"] = json!({"code":"fixture_error","message":"status failed"});
        message["result"] = json!({"detail":"unchanged","ownership":{"status":"verified_chrome","kernelPeerPid":1}});
    });
    expected["result"]
        .as_object_mut()
        .unwrap()
        .remove("ownership");
    assert_eq!(delivered.unwrap(), expected);
    let (read, _) = terminal_reply(&bridge, &mut host, "read", |_| {});
    assert_eq!(read.unwrap()["route"]["documentId"], "document-7");
    host.close();
    drop(bridge);
}

fn assert_ambiguous_status_strips_ownership(echoed_action: &str) {
    let endpoint = Endpoint::new();
    let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
    let mut host = paired_host(&endpoint, &bridge);
    let (delivered, mut expected) = terminal_reply(&bridge, &mut host, "status", |message| {
        message["status"] = json!("ambiguous");
        message["action"] = json!(echoed_action);
        message["error"] = json!({"code":"fixture_ambiguous","message":"route was revoked"});
        message["result"] = json!({"detail":"unchanged","ownership":{"status":"verified_chrome","kernelPeerPid":1}});
    });
    expected["result"]
        .as_object_mut()
        .unwrap()
        .remove("ownership");
    assert_eq!(delivered.unwrap(), expected, "echoed {echoed_action}");
    assert_disconnected(&bridge, &mut host);
    let replacement = Host::spawn(&endpoint, false);
    let driver = worker(replacement, &["status"]);
    ready(&bridge);
    assert!(bridge
        .read(None, None)
        .unwrap_err()
        .to_string()
        .contains("not paired"));
    driver.join().unwrap().close();
    drop(bridge);
}

#[test]
fn ambiguous_status_strips_forged_ownership_and_revokes_the_paired_route() {
    assert_ambiguous_status_strips_ownership("status");
}

#[test]
fn ambiguous_status_strips_forged_ownership_even_when_worker_echoes_another_action() {
    assert_ambiguous_status_strips_ownership("pair");
}

#[test]
fn non_success_status_preserves_optional_and_non_object_result_payloads() {
    for status in ["error", "ambiguous"] {
        for payload in [
            None,
            Some(Value::Null),
            Some(json!("detail")),
            Some(json!([1, 2])),
        ] {
            let endpoint = Endpoint::new();
            let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
            let mut host = paired_host(&endpoint, &bridge);
            let (delivered, expected) = terminal_reply(&bridge, &mut host, "status", |message| {
                message["status"] = json!(status);
                message["error"] = json!({"code":"fixture_error","message":"unchanged"});
                if let Some(payload) = payload {
                    message["result"] = payload;
                } else {
                    message.as_object_mut().unwrap().remove("result");
                }
            });
            assert_eq!(delivered.unwrap(), expected);
            if status == "ambiguous" {
                assert_disconnected(&bridge, &mut host);
            } else {
                host.close();
            }
            drop(bridge);
        }
    }
}

#[test]
fn invalid_terminal_identity_is_acknowledged_and_rejected_without_delivery() {
    for wrong_request in [false, true] {
        let endpoint = Endpoint::new();
        let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
        let mut host = paired_host(&endpoint, &bridge);
        let (delivered, _) = terminal_reply(&bridge, &mut host, "status", |message| {
            if wrong_request {
                message["requestId"] = json!("another-request");
                message["status"] = json!("ambiguous");
            } else {
                message["action"] = json!("pair");
            }
        });
        let error = delivered.unwrap_err().to_string();
        assert!(
            error.contains("ambiguous Chrome result identity"),
            "{error}"
        );
        assert_disconnected(&bridge, &mut host);
        drop(bridge);
    }
}

#[test]
fn successful_status_requires_an_object_after_its_receipt_is_acknowledged() {
    let endpoint = Endpoint::new();
    let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
    let mut host = paired_host(&endpoint, &bridge);
    let (delivered, _) = terminal_reply(&bridge, &mut host, "status", |message| {
        message["result"] = Value::Null;
    });
    let error = delivered.unwrap_err().to_string();
    assert!(
        error.contains("Chrome status result must be an object"),
        "{error}"
    );
    assert_disconnected(&bridge, &mut host);
    drop(bridge);
}

#[test]
fn actual_host_identity_is_broker_owned_live_and_replaced_on_reconnect() {
    let endpoint = Endpoint::new();
    let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
    let host = Host::spawn(&endpoint, false);
    let pid = host.child.id();
    let driver = worker(host, &["status", "status", "pair", "release", "status"]);
    let first = ready(&bridge);
    assert_eq!(first["status"], "process_relationship_observed");
    assert_eq!(first["kernelPeerPid"], pid);
    assert_eq!(first["host"]["pid"], pid);
    assert_eq!(first["parentCandidate"]["pid"], std::process::id());
    assert_eq!(first["launchKind"], "direct_parent");
    assert_eq!(first["browserIdentity"], "unproven");
    assert_eq!(first["nativeWindowAssociation"], "unproven");
    assert_eq!(
        ready(&bridge),
        first,
        "same live instance must keep the same witness"
    );
    bridge.pair().unwrap();
    bridge.release().unwrap();
    assert_eq!(
        ready(&bridge),
        first,
        "release only clears pairing eligibility"
    );
    let mut host = driver.join().unwrap();
    host.close();
    let replacement = Host::spawn(&endpoint, false);
    let next_pid = replacement.child.id();
    let driver = worker(replacement, &["status"]);
    let next = ready(&bridge);
    assert_eq!(next["host"]["pid"], next_pid);
    assert_ne!(
        next["host"], first["host"],
        "a new Session must not inherit an old instance"
    );
    let mut replacement = driver.join().unwrap();
    replacement.close();
    drop(bridge);
}

#[cfg(windows)]
#[test]
fn windows_real_system_cmd_hop_is_observed_without_claiming_chrome() {
    let endpoint = Endpoint::new();
    let bridge = ChromeBridge::bind(&endpoint.0).unwrap();
    let host = Host::spawn(&endpoint, true);
    let launcher_pid = host.child.id();
    let driver = worker(host, &["status"]);
    let status = ready(&bridge);
    assert_eq!(status["status"], "process_relationship_observed");
    assert_eq!(status["launchKind"], "windows_cmd_parent");
    assert_eq!(status["intermediate"]["pid"], launcher_pid);
    assert_eq!(
        status["intermediate"]["image"]
            .as_str()
            .unwrap()
            .to_lowercase(),
        "cmd.exe"
    );
    assert_eq!(status["parentCandidate"]["pid"], std::process::id());
    assert_ne!(status["kernelPeerPid"], launcher_pid);
    assert_eq!(status["host"]["pid"], status["kernelPeerPid"]);
    assert_eq!(status["browserIdentity"], "unproven");
    assert_eq!(status["nativeWindowAssociation"], "unproven");
    let mut host = driver.join().unwrap();
    host.close();
    drop(bridge);
}
