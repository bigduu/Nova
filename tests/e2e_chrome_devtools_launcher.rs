//! Chrome DevTools MCP sidecar regressions.
//!
//! The default tests are hermetic: a tiny fake `npx` proves Nova preserves
//! stdio, passes a literal audited argv, and validates incompatible policy
//! options before starting another process. The real upstream MCP handshake is
//! opt-in because it needs Node/npm and may download the pinned package.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[cfg(unix)]
struct Fixture {
    directory: std::path::PathBuf,
}

#[cfg(unix)]
impl Fixture {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "nova-chrome-devtools-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).expect("create launcher fixture directory");
        Self { directory }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.directory.join(name)
    }

    fn script(&self, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = self.path("fake-npx");
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).expect("write fake npx");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("make fake npx executable");
        path
    }
}

#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(unix)]
#[test]
fn fake_npx_receives_literal_safe_argv_and_inherits_stdio() {
    let fixture = Fixture::new("argv");
    let argv = fixture.path("argv");
    let environment = fixture.path("environment");
    let process_id = fixture.path("process-id");
    let fake_npx = fixture.script(
        r#"printf '%s\n' "$@" > "$NOVA_TEST_ARGV"
printf '%s\n%s\n' "$CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS" "$CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS" > "$NOVA_TEST_ENVIRONMENT"
printf '%s\n' "$$" > "$NOVA_TEST_PID"
IFS= read -r mcp_input
printf 'fake-devtools-stdout:%s\n' "$mcp_input"
printf 'fake-devtools-stderr\n' >&2"#,
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
        .args([
            "chrome-devtools",
            "--npx",
            fake_npx.to_str().unwrap(),
            "--headless",
            "--enable-webmcp",
            "--allowed-url-pattern",
            "https://example.com/*",
        ])
        .env("NOVA_TEST_ARGV", &argv)
        .env("NOVA_TEST_ENVIRONMENT", &environment)
        .env("NOVA_TEST_PID", &process_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run Nova Chrome DevTools launcher");
    let nova_process_id = child.id();
    writeln!(child.stdin.take().unwrap(), "mcp-input").unwrap();
    let output = child.wait_with_output().expect("wait for fake npx");

    assert!(
        output.status.success(),
        "launcher failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"fake-devtools-stdout:mcp-input\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("fake-devtools-stderr"),
        "fake npx stderr was not inherited: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(argv).unwrap(),
        concat!(
            "--yes\n",
            "chrome-devtools-mcp@1.8.0\n",
            "--isolated\n",
            "--no-usage-statistics\n",
            "--no-performance-crux\n",
            "--redact-network-headers\n",
            "--headless\n",
            "--category-experimental-webmcp=true\n",
            "--chrome-arg=--enable-features=WebMCP\n",
            "--allowed-url-pattern=https://example.com/*\n",
        )
    );
    assert_eq!(std::fs::read_to_string(environment).unwrap(), "1\n1\n");
    assert_eq!(
        std::fs::read_to_string(process_id)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap(),
        nova_process_id,
        "Unix launcher forked a proxy instead of replacing itself with npx"
    );
}

#[cfg(unix)]
#[test]
fn existing_profile_with_headless_is_rejected_before_npx() {
    let fixture = Fixture::new("invalid");
    let called = fixture.path("called");
    let fake_npx = fixture.script(r#"touch "$NOVA_TEST_CALLED""#);

    let output = Command::new(env!("CARGO_BIN_EXE_nova"))
        .args([
            "chrome-devtools",
            "--npx",
            fake_npx.to_str().unwrap(),
            "--profile",
            "existing",
            "--headless",
        ])
        .env("NOVA_TEST_CALLED", &called)
        .output()
        .expect("run invalid launcher invocation");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--headless cannot be used with --profile existing"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!called.exists(), "invalid options still invoked npx");
}

#[cfg(unix)]
#[test]
fn local_endpoint_modes_preserve_literal_argv_privacy_and_stdio() {
    for (flag, endpoint) in [
        ("--browser-url", "http://127.0.0.1:9222"),
        ("--browser-url", "https://[0:0:0:0:0:0:0:1]:443/"),
        (
            "--ws-endpoint",
            "ws://127.0.0.1:9222/devtools/browser/browser-1",
        ),
        (
            "--ws-endpoint",
            "wss://[::1]:443/devtools/browser/browser_2",
        ),
    ] {
        let fixture = Fixture::new("endpoint");
        let argv = fixture.path("argv");
        let environment = fixture.path("environment");
        let fake_npx = fixture.script(
            r#"printf '%s\n' "$@" > "$NOVA_TEST_ARGV"
printf '%s\n%s\n' "$CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS" "$CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS" > "$NOVA_TEST_ENVIRONMENT"
IFS= read -r mcp_input
printf 'fake-endpoint-stdout:%s\n' "$mcp_input"
printf 'fake-endpoint-stderr\n' >&2"#,
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
            .args([
                "chrome-devtools",
                "--npx",
                fake_npx.to_str().unwrap(),
                flag,
                endpoint,
                "--enable-webmcp",
            ])
            .env("NOVA_TEST_ARGV", &argv)
            .env("NOVA_TEST_ENVIRONMENT", &environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run fake endpoint launcher");
        writeln!(child.stdin.take().unwrap(), "mcp-input").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"fake-endpoint-stdout:mcp-input\n");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("fake-endpoint-stderr"));
        assert!(stderr.contains("cannot add launch flags to a running browser"));
        assert_eq!(std::fs::read_to_string(environment).unwrap(), "1\n1\n");
        assert_eq!(
            std::fs::read_to_string(argv).unwrap(),
            format!("--yes\nchrome-devtools-mcp@1.8.0\n{flag}\n{endpoint}\n--no-usage-statistics\n--no-performance-crux\n--redact-network-headers\n--category-experimental-webmcp=true\n")
        );
    }
}

#[cfg(unix)]
#[test]
fn endpoint_conflicts_fail_before_spawn_without_echoing_credentials() {
    let fixture = Fixture::new("endpoint-conflicts");
    let called = fixture.path("called");
    let fake_npx = fixture.script(r#"touch "$NOVA_TEST_CALLED""#);
    for (flag, endpoint) in [
        ("--browser-url", "http://private:secret@127.0.0.1:9222"),
        (
            "--ws-endpoint",
            "ws://private:secret@[::1]:9222/devtools/browser/id",
        ),
    ] {
        for extra in [
            vec!["--profile", "isolated"],
            vec!["--profile", "existing"],
            vec!["--headless"],
            vec![
                if flag == "--browser-url" {
                    "--ws-endpoint"
                } else {
                    "--browser-url"
                },
                endpoint,
            ],
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_nova"))
                .args([
                    "chrome-devtools",
                    "--npx",
                    fake_npx.to_str().unwrap(),
                    flag,
                    endpoint,
                ])
                .args(extra)
                .env("NOVA_TEST_CALLED", &called)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("choose only one") || stderr.contains("remove those options"));
            assert!(!stderr.contains("private") && !stderr.contains("secret"));
            assert!(!stderr.contains(endpoint));
            assert!(
                !called.exists(),
                "incompatible endpoint options invoked npx"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn malformed_or_nonlocal_endpoints_are_rejected_before_npx_without_raw_url_errors() {
    let fixture = Fixture::new("endpoint-syntax");
    let called = fixture.path("called");
    let fake_npx = fixture.script(r#"touch "$NOVA_TEST_CALLED""#);
    for (flag, endpoint) in [
        ("--browser-url", "http://localhost:9222"),
        ("--browser-url", "http://127.1:9222"),
        ("--browser-url", "http://2130706433:9222"),
        ("--browser-url", "http://0x7f000001:9222"),
        ("--browser-url", "http://0177.0.0.1:9222"),
        ("--browser-url", "http://192.168.1.1:9222"),
        ("--browser-url", "http://127.0.0.1"),
        ("--browser-url", "http://127.0.0.1:0"),
        ("--browser-url", "http://127.0.0.1:65536"),
        ("--browser-url", "http://private:secret@127.0.0.1:9222"),
        ("--browser-url", "http://127.0.0.1:9222/?private=secret"),
        ("--browser-url", "http://127.0.0.1:9222/#private-secret"),
        ("--browser-url", "http://127.0.0.1:9222/json/version"),
        ("--browser-url", "http://127.0.0.1:9222/../"),
        ("--browser-url", "http://127.0.0.1:9222\n"),
        ("--browser-url", "ws://127.0.0.1:9222/"),
        ("--browser-url", ""),
        ("--ws-endpoint", "http://127.0.0.1:9222/devtools/browser/id"),
        ("--ws-endpoint", "ws://[::1]:0/devtools/browser/id"),
        ("--ws-endpoint", "ws://[::1]:9222/devtools/page/id"),
        ("--ws-endpoint", "ws://[::1]:9222/node-inspector-id"),
        ("--ws-endpoint", "ws://[::1]:9222/devtools/browser/"),
        (
            "--ws-endpoint",
            "ws://[::1]:9222/devtools/browser/../page/id",
        ),
        (
            "--ws-endpoint",
            "ws://private:secret@[::1]:9222/devtools/browser/id",
        ),
        (
            "--ws-endpoint",
            "ws://[::1]:9222/devtools/browser/id?private=secret",
        ),
        (
            "--ws-endpoint",
            "ws://[::1]:9222/devtools/browser/id#private-secret",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_nova"))
            .args([
                "chrome-devtools",
                "--npx",
                fake_npx.to_str().unwrap(),
                flag,
                endpoint,
            ])
            .env("NOVA_TEST_CALLED", &called)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("literal loopback IP") && stderr.contains("nonzero port"));
        assert!(!stderr.contains("private") && !stderr.contains("secret"));
        assert!(!stderr.contains(endpoint) || endpoint.is_empty());
        assert!(!called.exists(), "invalid endpoint invoked npx");
    }
}

#[test]
#[ignore = "requires Node/npm and may download chrome-devtools-mcp@1.8.0"]
fn pinned_upstream_accepts_hardened_webmcp_options_and_lists_expected_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nova"))
        .args([
            "chrome-devtools",
            "--headless",
            "--enable-webmcp",
            "--allowed-url-pattern",
            "https://example.com/*",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pinned Chrome DevTools MCP sidecar");

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"nova-test","version":"1"}}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{init}").unwrap();
        writeln!(stdin, "{initialized}").unwrap();
        writeln!(stdin, "{list}").unwrap();
    }
    drop(child.stdin.take());

    let mut stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stdout.read_to_string(&mut output);
        let _ = sender.send(output);
    });

    let output = match receiver.recv_timeout(Duration::from_secs(60)) {
        Ok(output) => output,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Chrome DevTools MCP did not close after stdin EOF within 60s");
        }
    };
    let status = child.wait().expect("wait for Chrome DevTools MCP");
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut stream) = child.stderr.take() {
            let _ = stream.read_to_string(&mut stderr);
        }
        panic!("Chrome DevTools MCP failed ({status}): {stderr}");
    }

    assert!(
        output.contains("\"serverInfo\""),
        "missing initialize: {output}"
    );
    assert!(output.contains("\"id\":2"), "missing tools/list: {output}");
    for tool in [
        "take_snapshot",
        "click",
        "list_network_requests",
        "performance_start_trace",
        "list_webmcp_tools",
    ] {
        assert!(
            output.contains(&format!("\"name\":\"{tool}\"")),
            "missing {tool}: {output}"
        );
    }
}
