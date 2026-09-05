use super::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

fn app(name: &str, bundle: &str) -> RunningApp {
    RunningApp {
        name: name.into(),
        bundle_id: Some(format!("test.{name}")),
        bundle: bundle.into(),
        pid: 42,
        runtime: "electron",
        evidence: vec!["Electron Framework executable".into()],
        issues: Vec::new(),
    }
}
fn owner() -> ProcessIdentity {
    ProcessIdentity {
        pid: 42,
        started_seconds: 1,
        started_micros: 2,
        executable: "/tmp/Fixture.app/Contents/MacOS/Fixture".into(),
    }
}
struct FixtureSource {
    apps: Vec<RunningApp>,
    investigation: Investigation,
    owned_checks: AtomicUsize,
    valid_checks: usize,
    native: bool,
}
impl Source for FixtureSource {
    fn apps(&self, _: Option<&str>, _: Instant) -> (Vec<RunningApp>, bool) {
        (self.apps.clone(), false)
    }
    fn native_available(&self) -> bool {
        self.native
    }
    fn investigate(&self, _: &RunningApp, _: Instant) -> Investigation {
        self.investigation.clone()
    }
    fn ownership(&self, _: &Candidate, _: Instant) -> Ownership {
        if self.owned_checks.fetch_add(1, Ordering::SeqCst) < self.valid_checks {
            Ownership::Current
        } else {
            Ownership::Stale
        }
    }
}
fn source(candidates: Vec<Candidate>) -> FixtureSource {
    FixtureSource {
        apps: vec![app("Fixture", "/tmp/Fixture.app")],
        investigation: Investigation {
            candidates,
            ..Default::default()
        },
        owned_checks: AtomicUsize::new(0),
        valid_checks: usize::MAX,
        native: true,
    }
}

#[test]
fn zero_listener_candidates_and_default_privacy_are_preserved() {
    let mut source = source(Vec::new());
    source.investigation.processes.push(owner());
    let report = serde_json::to_value(inspect_with(&source, None, false)).unwrap();
    assert_eq!(report["apps"][0]["status"], "no_endpoint_discovered");
    assert_eq!(report["apps"][0]["native_route"], "ax_read");
    let encoded = report.to_string();
    for hidden in [
        "pid",
        "started_seconds",
        "bundle_path",
        "endpoint",
        "compatibility",
        "runtime_confidence",
    ] {
        assert!(!report["apps"][0].as_object().unwrap().contains_key(hidden));
    }
    assert!(!encoded.contains("debugging_disabled"));
    assert!(!encoded.contains("/tmp/"));
}

#[test]
fn exact_selector_and_helper_dedup_do_not_merge_other_apps() {
    let mut source = source(Vec::new());
    source.apps.push(app(
        "Fixture Helper",
        "/tmp/Fixture.app/Contents/Frameworks/Helper.app",
    ));
    source.apps.push(app("Other", "/tmp/Other.app"));
    assert_eq!(inspect_with(&source, None, false).apps.len(), 2);
    assert_eq!(
        inspect_with(&source, Some("test.Fixture"), false)
            .apps
            .len(),
        1
    );
    assert_eq!(
        inspect_with(&source, Some("does-not-exist"), false).status,
        "no_match"
    );
}

#[test]
fn denied_partial_and_unknown_states_never_claim_debugging_disabled() {
    let mut source = source(Vec::new());
    source.native = false;
    source.investigation.issues = vec![
        "process_inspection_denied".into(),
        "debug_flags_unavailable".into(),
    ];
    let report = inspect_with(&source, None, true);
    assert_eq!(report.status, "incomplete");
    assert_eq!(report.apps[0].inspection, "incomplete");
    assert_eq!(
        report.apps[0].native_route,
        "accessibility_permission_required"
    );
    assert!(report.apps[0].next_step.contains("enablement is unknown"));
    assert_eq!(
        report.apps[0].details.as_ref().unwrap().enablement,
        "unknown"
    );
}

#[derive(Clone, Copy)]
enum Behavior {
    Browser,
    Node,
    Renderer,
    Redirect,
    Oversized,
    Malformed,
    Remote,
    OtherPort,
    Events,
    SlowFragments,
    SlowHttp,
    LargeFrame,
}
struct Server {
    address: SocketAddr,
    methods: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn start(behavior: Behavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let methods = Arc::new(Mutex::new(Vec::new()));
        let recorded = methods.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut request_number = 0;
            while !stopping.load(Ordering::SeqCst) && Instant::now() < deadline {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                request_number += 1;
                if request_number == 1 {
                    serve_metadata(&mut stream, address, behavior);
                    continue;
                }
                let Ok(mut socket) = tungstenite::accept(stream) else {
                    continue;
                };
                if matches!(behavior, Behavior::LargeFrame) {
                    let _ = socket.send(tungstenite::Message::Text("x".repeat(40 * 1024).into()));
                    continue;
                }
                if matches!(behavior, Behavior::SlowFragments) {
                    let _ = socket.get_mut().write_all(&[0x01, 1, b'x']);
                    for _ in 0..100 {
                        if socket.get_mut().write_all(&[0x00, 1, b'x']).is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    continue;
                }
                for _ in 0..2 {
                    let Ok(tungstenite::Message::Text(text)) = socket.read() else {
                        break;
                    };
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let method = request["method"].as_str().unwrap();
                    recorded.lock().unwrap().push(method.into());
                    if matches!(behavior, Behavior::Events) {
                        for _ in 0..30 {
                            if socket
                                .send(tungstenite::Message::Ping(Vec::new().into()))
                                .is_err()
                            {
                                break;
                            }
                        }
                        break;
                    }
                    let result = if method == "Browser.getVersion" {
                        json!({"product":"Chrome/Test", "protocolVersion":"1.3"})
                    } else {
                        json!({"browserContextIds":[]})
                    };
                    let reply = if matches!(behavior, Behavior::Renderer)
                        && method == "Target.getBrowserContexts"
                    {
                        json!({"id":request["id"], "error":{"code":-32601,"message":"not browser scope"}})
                    } else {
                        json!({"id":request["id"], "result":result})
                    };
                    if socket
                        .send(tungstenite::Message::Text(reply.to_string().into()))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Self {
            address,
            methods,
            stop,
            thread: Some(thread),
        }
    }
    fn candidate(&self) -> Candidate {
        Candidate {
            address: self.address,
            owner: owner(),
            provenance: vec!["process_owned_listener".into()],
            expected_path: None,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn serve_metadata(stream: &mut TcpStream, address: SocketAddr, behavior: Behavior) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "GET /json/version HTTP/1.1\r\n");
    for _ in 0..30 {
        line.clear();
        if reader.read_line(&mut line).is_err() || line == "\r\n" {
            break;
        }
    }
    let host = if matches!(behavior, Behavior::Remote) {
        "example.com"
    } else {
        "127.0.0.1"
    };
    let port = if matches!(behavior, Behavior::OtherPort) {
        if address.port() == 65535 {
            65534
        } else {
            address.port() + 1
        }
    } else {
        address.port()
    };
    let body = match behavior {
        Behavior::Malformed => "not-json".to_string(),
        Behavior::Oversized => "x".repeat(40 * 1024),
        _ => json!({"Browser":if matches!(behavior, Behavior::Node) { "node.js/v24" } else { "Chrome/Test" }, "Protocol-Version":"1.3", "webSocketDebuggerUrl":format!("ws://{host}:{port}/devtools/browser/fixture")}).to_string(),
    };
    let status = if matches!(behavior, Behavior::Redirect) {
        "302 Found\r\nLocation: http://example.com/private"
    } else {
        "200 OK"
    };
    if matches!(behavior, Behavior::SlowHttp) {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        for byte in body.bytes() {
            if stream.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        return;
    }
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

#[test]
fn browser_handshake_requires_browser_scope_and_never_reads_page_content() {
    let server = Server::start(Behavior::Browser);
    let report = inspect_with(&source(vec![server.candidate()]), None, true);
    assert_eq!(report.apps[0].status, "browser_endpoint_available");
    assert_eq!(
        report.apps[0].details.as_ref().unwrap().browser_tools,
        "not_attached_compatibility_unverified"
    );
    assert_eq!(
        *server.methods.lock().unwrap(),
        ["Browser.getVersion", "Target.getBrowserContexts"]
    );
}

#[test]
fn metadata_or_renderer_node_inspectors_do_not_prove_browser_control() {
    for (behavior, expected) in [
        (Behavior::Node, "node_inspector_only"),
        (Behavior::Renderer, "incompatible_endpoint"),
    ] {
        let server = Server::start(behavior);
        assert_eq!(
            inspect_with(&source(vec![server.candidate()]), None, true).apps[0].status,
            expected
        );
    }
}

#[test]
fn ownership_is_rechecked_after_handshake_and_stale_file_paths_fail_closed() {
    let server = Server::start(Behavior::Browser);
    let mut source = source(vec![server.candidate()]);
    source.valid_checks = 1;
    let report = inspect_with(&source, None, true);
    assert_eq!(report.apps[0].status, "stale_endpoint_evidence");
    assert!(report.apps[0].details.as_ref().unwrap().endpoints[0]
        .product
        .is_none());
    let server = Server::start(Behavior::Browser);
    let mut candidate = server.candidate();
    candidate.expected_path = Some("/devtools/browser/old".into());
    assert_eq!(
        probe::verify(&candidate, Instant::now() + TOTAL_BUDGET).status,
        "stale_evidence"
    );
    assert!(server.methods.lock().unwrap().is_empty());
}

#[test]
fn hostile_endpoints_and_slow_fragment_trickles_are_bounded() {
    for behavior in [
        Behavior::Redirect,
        Behavior::Oversized,
        Behavior::Malformed,
        Behavior::Remote,
        Behavior::OtherPort,
        Behavior::Events,
        Behavior::SlowFragments,
        Behavior::SlowHttp,
        Behavior::LargeFrame,
    ] {
        let server = Server::start(behavior);
        let started = Instant::now();
        let result = probe::verify(&server.candidate(), Instant::now() + TOTAL_BUDGET);
        assert_ne!(result.status, "browser_handshake_verified");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn probe_deadline_keeps_app_and_overall_inspection_incomplete() {
    let server = Server::start(Behavior::SlowHttp);
    let report = inspect_with(&source(vec![server.candidate()]), None, true);
    assert_eq!(report.status, "incomplete");
    assert_eq!(report.apps[0].inspection, "incomplete");
    assert_eq!(report.apps[0].status, "endpoint_unverified");
    let details = report.apps[0].details.as_ref().unwrap();
    assert_eq!(details.endpoints[0].status, "timed_out");
    assert!(details.issues.iter().any(|issue| issue == "probe_deadline"));
}

#[test]
fn terminated_owner_is_not_probed() {
    let server = Server::start(Behavior::Browser);
    let mut source = source(vec![server.candidate()]);
    source.valid_checks = 0;
    assert_eq!(
        inspect_with(&source, None, true).apps[0].status,
        "stale_endpoint_evidence"
    );
    assert!(server.methods.lock().unwrap().is_empty());
}
