//! Real main-thread/resident-process regression. The temporary AppKit fixture
//! has no UI and requests no TCC permissions. Its optional CDP listener is a
//! protocol simulator, not Chromium; real Electron acceptance is separate.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--fixture-app") {
        return mac::fixture_main();
    }
    mac::regression()
}

#[cfg(target_os = "macos")]
mod mac {
    use anyhow::{Context, Result};
    use nova::app_inspection::{inspect, Inspection};
    use nova::platform::mac::event_loop;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub fn regression() -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let fixture = Fixture::new()?;
        // Unlike the ordinary #[test] harness, this is the OS main thread.
        assert!(objc2::MainThreadMarker::new().is_some());
        let service = async move { tokio::task::spawn_blocking(move || exercise(fixture)).await? };
        if std::env::args().any(|arg| arg == "--without-main-loop") {
            // Explicit negative control: reproduces the old entrypoint without
            // changing production code or the machine's installed Nova.
            return runtime.block_on(service);
        }
        event_loop::run(&runtime, service)?;
        // Completion and server failures must return through the main loop.
        let error = event_loop::run(&runtime, async { anyhow::bail!("fixture service error") })
            .unwrap_err();
        assert_eq!(error.to_string(), "fixture service error");
        let error =
            event_loop::run(&runtime, async { panic!("fixture service panic") }).unwrap_err();
        assert!(error.to_string().contains("desktop service task failed"));
        let error = std::thread::spawn(|| {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            event_loop::run(&runtime, async { Ok(()) }).unwrap_err()
        })
        .join()
        .unwrap();
        assert!(error.to_string().contains("process main thread"));
        println!("resident app inspection: launch/exit/relaunch, native + simulated CDP passed");
        Ok(())
    }

    fn exercise(mut fixture: Fixture) -> Result<()> {
        assert_eq!(inspect(Some(&fixture.bundle_id), true).status, "no_match");
        let mut previous_identity = None;
        for cdp in [false, true] {
            for _ in 0..2 {
                fixture.launch(cdp)?;
                let result = wait_for(&fixture.bundle_id, |r| r.apps.len() == 1)?;
                let app = &result.apps[0];
                assert_eq!(app.bundle_id.as_deref(), Some(fixture.bundle_id.as_str()));
                assert_eq!(
                    app.status,
                    if cdp {
                        "browser_endpoint_available"
                    } else {
                        "no_endpoint_discovered"
                    }
                );
                // The fixture intentionally provides no Chromium bundle evidence.
                assert_eq!(app.runtime, "unknown");
                let details = serde_json::to_value(&app.details)?;
                let process = &details["processes"][0];
                assert_eq!(process["pid"], fixture.pid.unwrap());
                let identity = json!([
                    process["pid"],
                    process["started_seconds"],
                    process["started_micros"]
                ]);
                assert_ne!(previous_identity.as_ref(), Some(&identity));
                previous_identity = Some(identity);
                if cdp {
                    assert_eq!(
                        details["endpoints"][0]["product"],
                        "Fixture/ProtocolSimulator"
                    );
                }
                fixture.quit();
                let gone = wait_for(&fixture.bundle_id, |r| r.status == "no_match")?;
                assert!(
                    gone.apps.is_empty(),
                    "exited app must not retain endpoint evidence"
                );
            }
        }
        Ok(())
    }

    fn wait_for(bundle_id: &str, ready: impl Fn(&Inspection) -> bool) -> Result<Inspection> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let result = inspect(Some(bundle_id), true);
            if ready(&result) {
                return Ok(result);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "inventory did not update: {result:?}"
            );
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    struct Fixture {
        root: PathBuf,
        bundle: PathBuf,
        bundle_id: String,
        pid: Option<i32>,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let root =
                std::env::temp_dir().join(format!("nova-resident-fixture-{}", std::process::id()));
            std::fs::create_dir(&root).context("create unique test-owned directory")?;
            let fixture = Self {
                bundle: root.join("NovaDiscoveryFixture.app"),
                bundle_id: format!("dev.nova.acceptance.resident{}", std::process::id()),
                root,
                pid: None,
            };
            let contents = fixture.bundle.join("Contents");
            std::fs::create_dir_all(contents.join("MacOS"))?;
            std::fs::copy(std::env::current_exe()?, contents.join("MacOS/fixture"))?;
            std::fs::write(contents.join("Info.plist"), format!(
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>fixture</string><key>CFBundleIdentifier</key><string>{}</string><key>CFBundleName</key><string>NovaDiscoveryFixture</string><key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/></dict></plist>", fixture.bundle_id))?;
            Ok(fixture)
        }

        fn launch(&mut self, cdp: bool) -> Result<()> {
            let marker = self.root.join("ready.json");
            let _ = std::fs::remove_file(&marker);
            let status = Command::new("/usr/bin/open")
                .args(["-n", "-g"])
                .arg(&self.bundle)
                .arg("--args")
                .arg("--fixture-app")
                .arg(&marker)
                .arg(if cdp { "cdp" } else { "native" })
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            anyhow::ensure!(status.success(), "launch test-owned app failed");
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                if let Ok(bytes) = std::fs::read(&marker) {
                    if let Ok(ready) = serde_json::from_slice::<Value>(&bytes) {
                        self.pid = ready["pid"].as_i64().map(|p| p as i32);
                        anyhow::ensure!(self.pid.is_some_and(|p| p > 1), "invalid fixture PID");
                        return Ok(());
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            anyhow::bail!("test-owned app did not become ready")
        }

        fn quit(&mut self) {
            if let Some(pid) = self.pid.take() {
                // SAFETY: this PID was written by this test's freshly launched
                // unique bundle into its private readiness file; never a user app.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.quit();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub fn fixture_main() -> Result<()> {
        let marker = std::env::args_os()
            .nth(2)
            .context("fixture marker required")?;
        let app = NSApplication::sharedApplication(objc2::MainThreadMarker::new().unwrap());
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        if std::env::args().nth(3).as_deref() == Some("cdp") {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            std::thread::spawn(move || serve_cdp(listener));
        }
        // Never leave a launched fixture behind if its controller fails early.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            std::process::exit(0);
        });
        std::fs::write(
            Path::new(&marker),
            json!({"pid":std::process::id()}).to_string(),
        )?;
        app.run();
        Ok(())
    }

    fn serve_cdp(listener: TcpListener) {
        for stream in listener.incoming().flatten() {
            let _ = reply_cdp(stream);
        }
    }

    fn reply_cdp(mut stream: TcpStream) -> Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut first = [0; 64];
        let read = stream.peek(&mut first)?;
        if first[..read].starts_with(b"GET /json/version ") {
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
            }
            let body = json!({"Browser":"Fixture/ProtocolSimulator", "webSocketDebuggerUrl":format!("ws://{}/devtools/browser/fixture", stream.local_addr()?)}).to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        } else {
            let mut socket = tungstenite::accept(stream)?;
            for method in ["Browser.getVersion", "Target.getBrowserContexts"] {
                let request: Value = serde_json::from_str(socket.read()?.to_text()?)?;
                anyhow::ensure!(request["method"] == method, "unexpected discovery method");
                let result = if method == "Browser.getVersion" {
                    json!({"product":"Fixture/ProtocolSimulator", "protocolVersion":"1.3"})
                } else {
                    json!({"browserContextIds":[]})
                };
                socket.send(tungstenite::Message::Text(
                    json!({"id":request["id"], "result":result})
                        .to_string()
                        .into(),
                ))?;
            }
        }
        Ok(())
    }
}
