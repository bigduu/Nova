#![cfg(windows)]

use nova_chrome_bridge::framing::{encode_native, read_native, MAX_MESSAGE_BYTES};
use nova_chrome_bridge::AppBridgeListener;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const ID: &str = "abcdefghijklmnopabcdefghijklmnop";
pub const DEADLINE: Duration = Duration::from_secs(5);

pub fn endpoint() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    PathBuf::from(format!(
        r"\\.\pipe\nova-chrome-process-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub fn hello() -> Value {
    json!({"protocolVersion":1,"kind":"hello","role":"chrome_extension","extensionId":ID})
}

pub fn route() -> Value {
    json!({"tabId":7,"documentId":"document-7","nonce":"page-7","epoch":3})
}

pub fn result(request: &Value, body: Value) -> Value {
    let mut result = json!({
        "protocolVersion":1,"kind":"result","requestId":request["requestId"],"action":request["action"],
        "status":"ok","epoch":3,"receipt":{"receiptId":format!("receipt-{}", request["requestId"].as_str().unwrap()),"expiresAt":10000},
        "result":body,
    });
    if request.get("route").is_some() {
        result["route"] = request["route"].clone();
    }
    result
}

fn binary() -> PathBuf {
    option_env!("CARGO_BIN_EXE_nova-chrome-host")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NOVA_TEST_CHROME_HOST").map(PathBuf::from))
        .expect("build the native host and provide NOVA_TEST_CHROME_HOST for root Windows tests")
}

pub struct Host {
    pub child: Child,
    pub input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
}

impl Host {
    pub fn spawn(path: &Path) -> Self {
        Self::with_origin(path, &format!("chrome-extension://{ID}/"), Some(ID))
    }

    pub fn with_origin(path: &Path, origin: &str, configured: Option<&str>) -> Self {
        let mut command = Command::new(binary());
        command
            .arg(origin)
            .env("NOVA_CHROME_PIPE", path)
            .env_remove("NOVA_CHROME_EXTENSION_ID");
        if let Some(id) = configured {
            command.env("NOVA_CHROME_EXTENSION_ID", id);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(value) =
                read_native(&mut stdout).expect("stdout must contain only binary native frames")
            {
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

    pub fn send(&mut self, value: &Value) {
        self.bytes(&encode_native(value).unwrap());
    }
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.input.as_mut().unwrap().write_all(bytes).unwrap();
    }
    pub fn receive(&self) -> Value {
        self.output
            .recv_timeout(DEADLINE)
            .expect("native frame must arrive while stdin is open")
    }
    pub fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "host did not exit while stdin remained open"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn stderr(&mut self) -> String {
        let mut diagnostic = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostic)
            .unwrap();
        diagnostic
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn accept(listener: &AppBridgeListener) -> nova_chrome_bridge::AppBridgeConnection {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(connection) = listener.try_accept().unwrap() {
            return connection;
        }
        assert!(Instant::now() < deadline, "launched host did not connect");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn windows_launched_host_preserves_fragmented_concatenated_and_binary_frames() {
    let listener = AppBridgeListener::bind(endpoint()).unwrap();
    let mut host = Host::spawn(listener.path());
    let mut app = accept(&listener);
    assert_eq!(app.receive().unwrap().unwrap()["extensionId"], ID);
    std::thread::sleep(Duration::from_millis(650));
    assert!(
        host.child.try_wait().unwrap().is_none(),
        "idle stdin must stay open"
    );
    let encoded = encode_native(&hello()).unwrap();
    host.bytes(&encoded[..2]);
    std::thread::sleep(Duration::from_millis(25));
    host.bytes(&encoded[2..6]);
    std::thread::sleep(Duration::from_millis(25));
    let event = json!({"protocolVersion":1,"kind":"event","name":"route_revoked","epoch":3});
    host.bytes(&[&encoded[6..], &encode_native(&event).unwrap()].concat());
    assert_eq!(app.receive().unwrap(), Some(hello()));
    assert_eq!(app.receive().unwrap(), Some(event.clone()));
    std::thread::sleep(Duration::from_millis(650));
    assert!(
        host.child.try_wait().unwrap().is_none(),
        "completed frame must clear its deadline"
    );
    // Native prefix contains both LF and Ctrl-Z: CRT text I/O would corrupt it.
    let mut binary = event;
    binary["padding"] = json!("");
    let base = serde_json::to_vec(&binary).unwrap().len();
    binary["padding"] = json!("x".repeat(0x1a0a - base));
    assert_eq!(&encode_native(&binary).unwrap()[..4], &[0x0a, 0x1a, 0, 0]);
    app.send(&binary).unwrap();
    assert_eq!(host.receive(), binary);
    drop(app);
    assert!(host.wait().success());
    assert!(host.input.is_some());
}

#[test]
fn windows_launched_host_rejects_missing_invalid_and_different_extension_origins() {
    let listener = AppBridgeListener::bind(endpoint()).unwrap();
    for (origin, configured) in [
        (format!("chrome-extension://{ID}/"), None),
        (format!("chrome-extension://{ID}/"), Some("bad")),
        ("https://example.test/".into(), Some(ID)),
        (
            "chrome-extension://pppppppppppppppppppppppppppppppp/".into(),
            Some(ID),
        ),
    ] {
        let mut host = Host::with_origin(listener.path(), &origin, configured);
        assert!(!host.wait().success());
        assert!(!host.stderr().is_empty());
        assert!(
            listener.try_accept().unwrap().is_none(),
            "origin rejection must happen before connecting"
        );
    }
}

#[test]
fn windows_launched_host_rejects_a_mismatched_extension_handshake() {
    let listener = AppBridgeListener::bind(endpoint()).unwrap();
    let mut host = Host::spawn(listener.path());
    let mut app = accept(&listener);
    assert_eq!(app.receive().unwrap().unwrap()["kind"], "host_hello");
    let mut wrong = hello();
    wrong["extensionId"] = json!("pppppppppppppppppppppppppppppppp");
    host.send(&wrong);
    assert!(!host.wait().success());
    assert!(host.stderr().contains("handshake identity"));
    assert!(app.receive().unwrap().is_none());
}

#[test]
fn windows_launched_host_rejects_zero_oversize_malformed_and_truncated_frames() {
    for bytes in [
        0u32.to_le_bytes().to_vec(),
        ((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes().to_vec(),
        vec![1, 0, 0, 0, b'{'],
        vec![10, 0, 0, 0, b'{'],
    ] {
        let listener = AppBridgeListener::bind(endpoint()).unwrap();
        let mut host = Host::spawn(listener.path());
        let mut app = accept(&listener);
        assert_eq!(app.receive().unwrap().unwrap()["kind"], "host_hello");
        host.bytes(&bytes);
        drop(host.input.take());
        assert!(!host.wait().success());
        assert!(!host.stderr().is_empty());
    }
}

#[test]
fn windows_launched_host_expires_held_open_prefix_body_and_drip_fed_input() {
    for (bytes, drip) in [
        (vec![10, 0], false),
        (vec![10, 0, 0, 0, b'{'], false),
        (vec![100, 0, 0, 0, b'{'], true),
    ] {
        let listener = AppBridgeListener::bind(endpoint()).unwrap();
        let mut host = Host::spawn(listener.path());
        let mut app = accept(&listener);
        assert_eq!(app.receive().unwrap().unwrap()["kind"], "host_hello");
        let started = Instant::now();
        host.bytes(&bytes);
        if drip {
            while started.elapsed() < Duration::from_secs(2)
                && host.child.try_wait().unwrap().is_none()
            {
                std::thread::sleep(Duration::from_millis(75));
                if host.input.as_mut().unwrap().write_all(b" ").is_err() {
                    break;
                }
            }
        }
        assert!(!host.wait().success());
        assert!(host.input.is_some(), "stdin must remain held open");
        assert!(host.stderr().contains("deadline"));
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(app.receive().unwrap().is_none());
    }
}

#[test]
fn windows_launched_host_cancels_an_unread_stdout_write_on_deadline() {
    let listener = AppBridgeListener::bind(endpoint()).unwrap();
    let child = Command::new(binary())
        .arg(format!("chrome-extension://{ID}/"))
        .env("NOVA_CHROME_EXTENSION_ID", ID)
        .env("NOVA_CHROME_PIPE", listener.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    struct Kill(Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Kill(child);
    let mut app = accept(&listener);
    assert_eq!(app.receive().unwrap().unwrap()["kind"], "host_hello");
    let started = Instant::now();
    app.send(&json!({"protocolVersion":1,"kind":"event","name":"route_revoked","epoch":3,"padding":"x".repeat(900_000)})).unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if Instant::now() >= deadline {
            panic!("unread native stdout pinned the host");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(started.elapsed() < DEADLINE);
}
