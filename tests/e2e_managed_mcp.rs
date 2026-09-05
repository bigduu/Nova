//! Managed-entrypoint transport tests without OS permission or app side effects.
//!
//! Every macOS connector gets an isolated NOVA_APP_SOCKET, which disables
//! LaunchServices. The test-owned listener runs only MCP initialize/list/ping;
//! it never starts Nova's app-service bootstrap or invokes desktop handlers.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(5);

struct Connector {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<String>,
}

impl Connector {
    fn spawn(arguments: &[&str], socket: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
            .args(arguments)
            // An override is mandatory: even a broken test must never launch
            // an installed Nova.app or use the user's service.
            .env("NOVA_APP_SOCKET", socket)
            .env_remove("NOVA_APP_BUNDLE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn connector");
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
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
            let line = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("MCP response must stream before stdin EOF");
            let value: Value =
                serde_json::from_str(&line).expect("stdout must contain only JSON-RPC");
            if value.get("id") == Some(&json!(id)) {
                return value;
            }
        }
    }

    fn handshake_and_ping(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "managed-mcp-test", "version": "1"}
            }
        }));
        let initialized = self.response(1);
        assert!(initialized["result"]["serverInfo"].is_object());
        assert_eq!(initialized["result"]["protocolVersion"], "2024-11-05");
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        self.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let tools = self.response(2);
        assert!(tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "ax_read"));
        // This tiny response must flush after the large tools/list response,
        // while both the app socket and the client's stdin remain open.
        self.send(json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}));
        assert_eq!(self.response(3)["result"], json!({}));
        assert!(self.child.try_wait().unwrap().is_none());
        assert!(self.input.is_some());
    }

    fn finish(&mut self) -> std::process::ExitStatus {
        drop(self.input.take());
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "connector failed to exit within deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "macos")]
    fn stderr(&mut self) -> String {
        use std::io::Read;
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        stderr
    }
}

impl Drop for Connector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Fixture {
        directory: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            // Keep below Darwin's 104-byte Unix socket path limit.
            let directory = PathBuf::from(format!(
                "/tmp/nova-mcp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            Self { directory }
        }

        fn socket(&self) -> PathBuf {
            self.directory.join("service.sock")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.socket());
            let _ = std::fs::remove_dir(&self.directory);
        }
    }

    struct TestService {
        fixture: Fixture,
        accepted: Arc<AtomicUsize>,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TestService {
        fn start() -> Self {
            let fixture = Fixture::new();
            let listener = std::os::unix::net::UnixListener::bind(fixture.socket()).unwrap();
            std::fs::set_permissions(fixture.socket(), std::fs::Permissions::from_mode(0o600))
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            let accepted = Arc::new(AtomicUsize::new(0));
            let count = accepted.clone();
            let (stop, stopping) = tokio::sync::oneshot::channel();
            let thread = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                    tokio::pin!(stopping);
                    loop {
                        tokio::select! {
                            _ = &mut stopping => break,
                            connection = listener.accept() => {
                                let (stream, _) = connection.unwrap();
                                count.fetch_add(1, Ordering::SeqCst);
                                tokio::spawn(async move {
                                    // Only protocol operations are sent. This
                                    // bypasses all OS/bootstrap/permission code.
                                    nova::server::run_unix_stream(stream).await.unwrap();
                                });
                            }
                        }
                    }
                });
            });
            Self {
                fixture,
                accepted,
                stop: Some(stop),
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestService {
        fn drop(&mut self) {
            let _ = self.stop.take().unwrap().send(());
            self.thread.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn explicit_connector_streams_from_test_owned_service() {
        let service = TestService::start();
        let mut connector = Connector::spawn(&["--connect"], &service.fixture.socket());
        connector.handshake_and_ping();
        assert!(connector.finish().success());
        assert_eq!(service.accepted.load(Ordering::SeqCst), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn managed_plugin_connectors_reuse_one_independent_service() {
        let manifest: Value =
            serde_json::from_str(include_str!("../packaging/plugin/plugin.json")).unwrap();
        let server = manifest["provides"]["mcp_servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|server| server["id"] == "nova")
            .unwrap();
        let arguments: Vec<&str> = server["transport"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|argument| argument.as_str().unwrap())
            .collect();
        assert_eq!(arguments, ["mcp"]);
        let service = TestService::start();
        let mut connector_ids = Vec::new();
        for expected_connections in 1..=2 {
            let mut connector = Connector::spawn(&arguments, &service.fixture.socket());
            connector_ids.push(connector.child.id());
            connector.handshake_and_ping();
            assert!(connector.finish().success());
            assert_eq!(
                service.accepted.load(Ordering::SeqCst),
                expected_connections
            );
            assert!(service.fixture.socket().exists());
        }
        assert_ne!(connector_ids[0], connector_ids[1]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unavailable_managed_service_fails_closed_with_recovery_guidance() {
        let fixture = Fixture::new();
        let mut connector = Connector::spawn(&["mcp"], &fixture.socket());
        // Keep stdin OPEN: falling back to direct stdio would stay alive and
        // fail this deadline, even without an initialize request.
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = connector.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "missing service did not fail closed"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(!status.success());
        assert!(
            connector.output.try_iter().next().is_none(),
            "error polluted MCP stdout"
        );
        let error = connector.stderr();
        for expected in [
            "Nova.app",
            "/Applications",
            "reconnect only",
            "Bodhi can remain open",
            "overridden socket",
        ] {
            assert!(error.contains(expected), "missing {expected:?}: {error}");
        }
        assert!(!fixture.socket().exists());
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn managed_mcp_retains_native_stdio_without_using_app_socket() {
    // A relative override is rejected by the app transport. Success proves
    // Windows/headless mcp kept the ordinary stdio path instead of --connect.
    let mut connector = Connector::spawn(&["mcp"], Path::new("not-an-app-socket"));
    connector.handshake_and_ping();
    assert!(connector.finish().success());
}
