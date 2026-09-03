//! App-service transport regressions.
//!
//! These tests use the real binary in hidden `--app-service` mode.  They need
//! no desktop session or TCC grants: only initialize/tools-list travel through
//! the same private Unix socket and `--connect` proxy used by Nova.app.

#![cfg(unix)]

use std::fs::Metadata;
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ServiceProcess {
    child: Child,
    socket: PathBuf,
}

impl ServiceProcess {
    fn spawn(label: &str) -> Self {
        let runtime_dir =
            std::env::temp_dir().join(format!("nova-app-e2e-{}-{label}", std::process::id()));
        let _ = std::fs::remove_file(runtime_dir.join("service.sock"));
        let _ = std::fs::remove_file(runtime_dir.join("service.lock"));
        let _ = std::fs::remove_file(runtime_dir.join("chrome.sock"));
        let _ = std::fs::remove_dir(&runtime_dir);
        let socket = runtime_dir.join("service.sock");
        let chrome_socket = runtime_dir.join("chrome.sock");
        let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg("--app-service")
            .env("NOVA_APP_SOCKET", &socket)
            .env("NOVA_CHROME_SOCKET", &chrome_socket)
            .env("NOVA_APP_ALLOW_UNBUNDLED_SERVICE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn app service");

        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            if let Some(status) = child.try_wait().expect("poll app service") {
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut stream| {
                        use std::io::Read;
                        let mut output = String::new();
                        let _ = stream.read_to_string(&mut output);
                        output
                    })
                    .unwrap_or_default();
                panic!("app service exited before binding ({status}): {stderr}");
            }
            assert!(
                Instant::now() < deadline,
                "app-service socket was not created"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Self { child, socket }
    }

    fn metadata(&self) -> Metadata {
        std::fs::symlink_metadata(&self.socket).expect("app-service socket metadata")
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let runtime_dir = self.socket.parent().unwrap();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(runtime_dir.join("service.lock"));
        let _ = std::fs::remove_file(runtime_dir.join("chrome.sock"));
        let _ = std::fs::remove_dir(runtime_dir);
    }
}

fn assert_private(path: &Path, metadata: &Metadata, expected_mode: u32) {
    // SAFETY: geteuid has no preconditions and no failure return.
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.mode() & 0o777, expected_mode);
    assert!(path.is_absolute());
}

#[test]
fn app_service_socket_is_private_and_singleton() {
    let mut service = ServiceProcess::spawn("singleton");
    let socket_metadata = service.metadata();
    assert!(socket_metadata.file_type().is_socket());
    assert_private(&service.socket, &socket_metadata, 0o600);

    let runtime_dir = service.socket.parent().unwrap();
    let directory_metadata = std::fs::symlink_metadata(runtime_dir).unwrap();
    assert!(directory_metadata.file_type().is_dir());
    assert_private(runtime_dir, &directory_metadata, 0o700);

    let duplicate = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("--app-service")
        .env("NOVA_APP_SOCKET", &service.socket)
        .env("NOVA_CHROME_SOCKET", runtime_dir.join("chrome.sock"))
        .env("NOVA_APP_ALLOW_UNBUNDLED_SERVICE", "1")
        .output()
        .expect("run duplicate app service");
    assert!(
        duplicate.status.success(),
        "duplicate app service failed: {}",
        String::from_utf8_lossy(&duplicate.stderr)
    );
    assert!(service.child.try_wait().unwrap().is_none());
    assert!(service.socket.exists(), "duplicate removed the live socket");
}

#[test]
fn connect_proxy_completes_mcp_handshake() {
    let service = ServiceProcess::spawn("handshake");
    let mut connector = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("--connect")
        .env("NOVA_APP_SOCKET", &service.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn app-service connector");

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"app-e2e","version":"1"}}}"#;
    let inited = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    {
        let stdin = connector.stdin.as_mut().unwrap();
        writeln!(stdin, "{init}").unwrap();
        writeln!(stdin, "{inited}").unwrap();
        writeln!(stdin, "{list}").unwrap();
    }
    drop(connector.stdin.take());

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if connector.try_wait().expect("poll connector").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = connector.kill();
            let _ = connector.wait();
            panic!("app-service connector did not close after stdin EOF");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = connector.wait_with_output().expect("wait for connector");
    assert!(
        output.status.success(),
        "connector failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"serverInfo\""),
        "missing initialize: {stdout}"
    );
    assert!(stdout.contains("\"id\":2"), "missing tools/list: {stdout}");
}
