//! End-to-end test for the LEGACY `--capture-worker` pipe protocol, which now
//! proxies into the shared capture daemon. Still-running nova servers from
//! pre-daemon builds spawn this entry point from the binary on disk, so the old
//! wire contract (JSON request line in, JSON header + raw RGB out) must keep
//! working. `#[ignore]`d by default. Run:
//!   cargo test --test e2e_worker -- --ignored --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

mod common;
use common::with_timeout;

/// Drive the proxy exactly like an old parent: write a request line, read the
/// header line. A window-name miss must come back as a CLEAN protocol error
/// (`ok:false` + a message), not a hang or a dead pipe.
#[test]
#[ignore = "spawns the legacy capture-worker proxy; needs the built nova binary"]
fn legacy_pipe_protocol_still_served() {
    let sock = format!("/tmp/nova-test-proxy-{}.sock", std::process::id());
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(format!("{sock}.lock"));

    let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
        .arg("--capture-worker")
        .env("NOVA_CAPTURE_BIN", env!("CARGO_BIN_EXE_nova"))
        .env("NOVA_CAPTURE_SOCK", &sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn legacy worker proxy");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    let header = with_timeout(60, "legacy proxy exchange", move || {
        writeln!(
            stdin,
            r#"{{"Window":{{"query":"__nova_no_such_window_zzzqx__"}}}}"#
        )
        .expect("write request");
        stdin.flush().expect("flush");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header");
        line
    });

    let v: serde_json::Value = serde_json::from_str(header.trim()).expect("header is JSON");
    assert_eq!(v["ok"], false, "window miss must be a clean error: {v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no on-screen window matching"),
        "error should be the window-miss message, got: {v}"
    );

    let _ = child.kill();
    let _ = child.wait();
    // Clean up the daemon the proxy spawned: connect to the same socket from
    // this process (the daemon's cmdline carries no socket path, so pgrep can't
    // find it) and kill the pid its handshake reports.
    std::env::set_var("NOVA_CAPTURE_BIN", env!("CARGO_BIN_EXE_nova"));
    std::env::set_var("NOVA_CAPTURE_SOCK", &sock);
    let c = nova::platform::mac::capture::broker::CaptureClient::new();
    let _ = c.capture(
        &nova::platform::mac::capture::broker::CaptureRequest::Window {
            query: "__nova_no_such_window_zzzqx__".to_string(),
        },
    );
    if let Some(pid) = c.daemon_pid() {
        // SAFETY: SIGKILL to the daemon this test caused to spawn.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(format!("{sock}.lock"));
}
