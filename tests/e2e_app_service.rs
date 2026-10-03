//! App-service transport regressions.
//!
//! These tests use the real binary in hidden `--app-service` mode.  They need
//! no desktop session or TCC grants: only initialize/tools-list travel through
//! the same private Unix socket and `--connect` proxy used by Nova.app.

#![cfg(unix)]

use std::fs::Metadata;
use std::io::{BufRead, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"app-e2e","version":"1"}}}"#;

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
        let child = Command::new(env!("CARGO_BIN_EXE_nova"))
            .arg("--app-service")
            .env("NOVA_APP_SOCKET", &socket)
            .env("NOVA_CHROME_SOCKET", &chrome_socket)
            .env("NOVA_APP_ALLOW_UNBUNDLED_SERVICE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn app service");

        // Establish ownership before any readiness wait can fail or unwind.
        Self { child, socket }
            .ready(Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn ready(mut self, timeout: Duration) -> Result<Self, String> {
        let result = wait_until_ready(&self.socket, timeout, || {
            match self.child.try_wait().map_err(|error| error.to_string())? {
                Some(status) => Err(format!("app service exited before readiness ({status})")),
                None => Ok(()),
            }
        });
        if let Err(error) = result {
            // Stop and reap our child before collecting diagnostics. Drop also
            // removes its files, including on an unexpected readiness panic.
            let _ = self.child.kill();
            let status = self
                .child
                .wait()
                .map(|status| status.to_string())
                .unwrap_or_else(|error| format!("wait failed: {error}"));
            let stderr = bounded_stderr(&mut self.child);
            return Err(format!(
                "{error}; child status: {status}; stderr (up to 8 KiB): {stderr}"
            ));
        }
        Ok(self)
    }

    fn metadata(&self) -> Metadata {
        std::fs::symlink_metadata(&self.socket).expect("app-service socket metadata")
    }
}

fn bounded_stderr(child: &mut Child) -> String {
    let Some(stderr) = child.stderr.take() else {
        return String::new();
    };
    // A descendant could still hold the pipe after our child terminates. Do
    // not let diagnostics extend the readiness deadline by blocking on EOF.
    let fd = stderr.as_raw_fd();
    // SAFETY: fd belongs to the live ChildStderr above; fcntl needs no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return "unavailable (could not make stderr nonblocking)".into();
    }
    let mut output = Vec::new();
    let _ = stderr.take(8192).read_to_end(&mut output);
    String::from_utf8_lossy(&output).into_owned()
}

fn wait_until_ready(
    socket: &Path,
    timeout: Duration,
    mut check_child: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create readiness runtime: {error}"))?;
    runtime.block_on(async {
        let initialize = async {
            let mut stream = loop {
                match tokio::net::UnixStream::connect(socket).await {
                    Ok(stream) => break stream,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        ) =>
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(error) => return Err(format!("connect app-service socket: {error}")),
                }
            };
            stream
                .write_all(format!("{INITIALIZE}\n").as_bytes())
                .await
                .map_err(|error| format!("write MCP initialize: {error}"))?;
            let mut reader = tokio::io::BufReader::new(stream).take(64 * 1024);
            let mut response = Vec::new();
            reader
                .read_until(b'\n', &mut response)
                .await
                .map_err(|error| format!("read MCP initialize response: {error}"))?;
            if response.last() != Some(&b'\n') {
                return Err("MCP initialize response missing or incomplete (64 KiB limit)".into());
            }
            let response: serde_json::Value = serde_json::from_slice(&response)
                .map_err(|error| format!("invalid MCP initialize JSON: {error}"))?;
            if response["jsonrpc"] != "2.0"
                || response["id"] != 1
                || response.get("error").is_some()
                || response["result"]["protocolVersion"] != "2024-11-05"
                || !response["result"]["serverInfo"]["name"].is_string()
                || !response["result"]["serverInfo"]["version"].is_string()
                || !response["result"]["capabilities"].is_object()
            {
                return Err("invalid MCP initialize response (expected successful id 1)".into());
            }

            // Binding alone is not readiness: chmod must precede the server's
            // initialize reply. Once it replies, insecure final modes fail.
            let socket_metadata = std::fs::symlink_metadata(socket)
                .map_err(|error| format!("final socket metadata: {error}"))?;
            if !socket_metadata.file_type().is_socket() {
                return Err("final app-service endpoint is not a socket".into());
            }
            require_private(socket, &socket_metadata, 0o600)?;
            let directory = socket.parent().ok_or("socket has no parent directory")?;
            let directory_metadata = std::fs::symlink_metadata(directory)
                .map_err(|error| format!("final directory metadata: {error}"))?;
            if !directory_metadata.file_type().is_dir() {
                return Err("final app-service parent is not a directory".into());
            }
            require_private(directory, &directory_metadata, 0o700)
        };
        let bounded = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), initialize);
        tokio::pin!(bounded);
        let mut poll = tokio::time::interval(Duration::from_millis(20));
        loop {
            tokio::select! {
                _ = poll.tick() => check_child()?,
                result = &mut bounded => return result.unwrap_or_else(|_| Err(format!(
                    "timed out after {timeout:?} waiting for MCP initialize at {}", socket.display()
                ))),
            }
        }
    })
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
    require_private(path, metadata, expected_mode).unwrap();
}

fn require_private(path: &Path, metadata: &Metadata, expected_mode: u32) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions and no failure return.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "{} is not owned by the effective UID",
            path.display()
        ));
    }
    let mode = metadata.mode() & 0o777;
    if mode != expected_mode {
        return Err(format!(
            "{} has final mode {mode:04o}, expected {expected_mode:04o}",
            path.display()
        ));
    }
    if !path.is_absolute() {
        return Err("app-service path must be absolute".into());
    }
    Ok(())
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

    let inited = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    {
        let stdin = connector.stdin.as_mut().unwrap();
        writeln!(stdin, "{INITIALIZE}").unwrap();
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

const INITIALIZE_RESPONSE: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"readiness-fixture","version":"1"}}}"#;

struct InitializeFixture {
    socket: PathBuf,
    request_seen: Receiver<()>,
    release_reply: Option<Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl InitializeFixture {
    fn spawn(label: &str, mode: u32, response: &'static str) -> Self {
        let directory = fixture_directory(label);
        let socket = directory.join("service.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(mode)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (request_tx, request_seen) = mpsc::channel();
        let (release_reply, reply_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept fixture initialize: {error}"),
                }
            };
            // macOS accepted sockets inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(request.trim_end(), INITIALIZE);
            if request_tx.send(()).is_ok() && reply_rx.recv_timeout(Duration::from_secs(5)).is_ok()
            {
                let _ = writeln!(stream, "{response}");
            }
        });
        Self {
            socket,
            request_seen,
            release_reply: Some(release_reply),
            worker: Some(worker),
        }
    }

    fn wait_for_request(&self) {
        self.request_seen
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
    }

    fn reply(&mut self) {
        self.release_reply.take().unwrap().send(()).unwrap();
    }
}

impl Drop for InitializeFixture {
    fn drop(&mut self) {
        // Release a blocked fixture even if an assertion failed.
        drop(self.release_reply.take());
        let _ = self.worker.take().unwrap().join();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(self.socket.parent().unwrap());
    }
}

fn fixture_directory(label: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("nova-app-ready-{}-{label}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[test]
fn readiness_waits_for_initialize_after_bind_and_chmod() {
    let mut fixture = InitializeFixture::spawn("chmod", 0o755, INITIALIZE_RESPONSE);
    let socket = fixture.socket.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        ready_tx
            .send(wait_until_ready(&socket, Duration::from_secs(3), || Ok(())))
            .unwrap();
    });
    fixture.wait_for_request();
    assert_eq!(
        std::fs::symlink_metadata(&fixture.socket).unwrap().mode() & 0o777,
        0o755
    );
    // These bounded channel checks prove non-return while the response is
    // held; elapsed time is never used to declare the service ready.
    assert_eq!(
        ready_rx.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    );

    std::fs::set_permissions(&fixture.socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    // Even correct permissions are insufficient without the held MCP reply.
    assert_eq!(
        ready_rx.recv_timeout(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    );
    fixture.reply();
    ready_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    waiter.join().unwrap();
}

#[test]
fn readiness_rejects_insecure_final_modes_after_valid_initialize() {
    for (label, socket_mode, directory_mode, expected) in [
        ("bad-socket", 0o755, 0o700, "expected 0600"),
        ("bad-directory", 0o600, 0o755, "expected 0700"),
    ] {
        let mut fixture = InitializeFixture::spawn(label, socket_mode, INITIALIZE_RESPONSE);
        std::fs::set_permissions(
            fixture.socket.parent().unwrap(),
            std::fs::Permissions::from_mode(directory_mode),
        )
        .unwrap();
        fixture.reply();
        let error =
            wait_until_ready(&fixture.socket, Duration::from_secs(3), || Ok(())).unwrap_err();
        fixture.wait_for_request();
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn readiness_rejects_unsuccessful_initialize() {
    let mut fixture = InitializeFixture::spawn(
        "invalid-response",
        0o600,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"fixture failure"}}"#,
    );
    fixture.reply();
    let error = wait_until_ready(&fixture.socket, Duration::from_secs(3), || Ok(())).unwrap_err();
    fixture.wait_for_request();
    assert!(error.contains("expected successful id 1"), "{error}");
}

#[test]
fn readiness_deadline_applies_while_initialize_reply_is_held() {
    let fixture = InitializeFixture::spawn("held-response", 0o600, INITIALIZE_RESPONSE);
    let error =
        wait_until_ready(&fixture.socket, Duration::from_millis(500), || Ok(())).unwrap_err();
    fixture.wait_for_request();
    assert!(error.contains("timed out after 500ms"), "{error}");
    assert!(error.contains("waiting for MCP initialize"), "{error}");
    // Dropping the fixture cancels its held response and joins the worker.
}

#[test]
fn readiness_failure_reaps_owned_child_and_removes_its_files() {
    for (label, script, timeout, expected) in [
        (
            "early-exit",
            "printf 'fixture early exit\\n' >&2; exit 7",
            Duration::from_secs(2),
            "exited before readiness",
        ),
        (
            "timeout",
            "exec sleep 60",
            Duration::from_millis(100),
            "timed out",
        ),
    ] {
        let directory = fixture_directory(label);
        std::fs::write(directory.join("service.lock"), b"fixture").unwrap();
        let child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let error = ServiceProcess {
            child,
            socket: directory.join("service.sock"),
        }
        .ready(timeout)
        .err()
        .expect("fixture must fail readiness");
        assert!(error.contains(expected), "{error}");
        if label == "early-exit" {
            assert!(error.contains("fixture early exit"), "{error}");
        }
        assert!(!directory.exists(), "failed readiness left owned files");
        // SAFETY: WNOHANG is nonblocking; a null status pointer is allowed.
        assert_eq!(
            unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
