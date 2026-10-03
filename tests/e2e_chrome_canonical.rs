//! Public JSON-RPC/MCP tests with the production server and broker. The terminal
//! peer below is a fixture, not Chrome or native-window association evidence.
#![cfg(any(target_os = "macos", windows))]

use nova::server::NovaServer;
use nova_chrome_bridge::{protocol::host_hello, ChromeBridge};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

const ID: &str = "abcdefghijklmnopabcdefghijklmnop";
const LIMIT: Duration = Duration::from_secs(25);

#[cfg(unix)]
struct Wire(std::io::BufReader<std::os::unix::net::UnixStream>);
#[cfg(unix)]
impl Wire {
    fn connect(path: &std::path::Path) -> Self {
        let stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        Self(std::io::BufReader::new(stream))
    }
    fn send(&mut self, message: &Value) {
        use std::io::Write;
        writeln!(self.0.get_mut(), "{message}").unwrap();
    }
    fn receive(&mut self, stop: &AtomicBool) -> Option<Value> {
        use std::io::BufRead;
        let mut line = String::new();
        while !stop.load(Ordering::Acquire) {
            match self.0.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => return Some(serde_json::from_str(&line).unwrap()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("fixture wire: {error}"),
            }
        }
        None
    }
}
#[cfg(windows)]
struct Wire(nova_chrome_bridge::AppBridgeConnection);
#[cfg(windows)]
impl Wire {
    fn connect(path: &std::path::Path) -> Self {
        Self(nova_chrome_bridge::AppBridgeConnection::connect(path).unwrap())
    }
    fn send(&mut self, message: &Value) {
        self.0.send(message).unwrap();
    }
    fn receive(&mut self, stop: &AtomicBool) -> Option<Value> {
        while !stop.load(Ordering::Acquire) {
            if self.0.wait_readable(Duration::from_millis(50)).unwrap() {
                return self.0.receive().unwrap();
            }
        }
        None
    }
}

enum Plan {
    Reply(Value),
    Held(mpsc::Receiver<Value>),
    Silence,
}
struct Fixture {
    bridge: Option<ChromeBridge>,
    plans: mpsc::Sender<Plan>,
    requests: mpsc::Receiver<Value>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"\\.\pipe\nova-canonical-{suffix}"));
        #[cfg(unix)]
        let path = {
            use std::os::unix::fs::PermissionsExt;
            let directory = std::env::temp_dir().join(format!("nova-canonical-{suffix}"));
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
            directory.join("chrome.sock")
        };
        let bridge = ChromeBridge::bind(&path).unwrap();
        let mut wire = Wire::connect(&path);
        let (plans, jobs) = mpsc::channel();
        let (events, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            wire.send(&host_hello(ID).unwrap());
            wire.send(&json!({"protocolVersion":1,"kind":"hello","role":"chrome_extension","extensionId":ID}));
            while let Some(request) = wire.receive(&stopped) {
                assert_eq!(request["kind"], "request");
                let body = match request["action"].as_str().unwrap() {
                    "status" => json!({"status":"ok","result":{"paired":false}}),
                    "pair" => {
                        json!({"status":"ok","result":{"route":{"tabId":7,"documentId":"document-7","nonce":"page-7","epoch":3}}})
                    }
                    "release" => json!({"status":"ok","result":{"released":true}}),
                    _ => {
                        events.send(request.clone()).unwrap();
                        match jobs.recv_timeout(LIMIT).unwrap() {
                            Plan::Reply(body) => body,
                            Plan::Held(release) => release.recv_timeout(LIMIT).unwrap(),
                            Plan::Silence => continue,
                        }
                    }
                };
                let mut terminal = json!({"protocolVersion":1,"kind":"result","requestId":request["requestId"],
                    "action":request["action"],"epoch":3,"receipt":{"receiptId":"fixture-receipt","expiresAt":10000}});
                if let Some(route) = request.get("route") {
                    terminal["route"] = route.clone();
                }
                terminal
                    .as_object_mut()
                    .unwrap()
                    .extend(body.as_object().unwrap().clone());
                wire.send(&terminal);
                let ack = wire
                    .receive(&stopped)
                    .expect("terminal requires its real broker receipt ACK");
                assert_eq!(
                    ack,
                    json!({"protocolVersion":1,"kind":"receipt","requestId":request["requestId"],
                    "action":terminal["action"],"epoch":3,"receiptId":"fixture-receipt"})
                );
            }
        });
        let deadline = Instant::now() + LIMIT;
        while bridge.status().is_err() {
            assert!(Instant::now() < deadline, "fixture handshake not ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            bridge: Some(bridge),
            plans,
            requests,
            stop,
            worker: Some(worker),
            #[cfg(unix)]
            path,
        }
    }
    fn reply(&self, body: Value) {
        self.plans.send(Plan::Reply(body)).unwrap();
    }
    fn hold(&self) -> mpsc::Sender<Value> {
        let (sender, receiver) = mpsc::channel();
        self.plans.send(Plan::Held(receiver)).unwrap();
        sender
    }
    fn next(&self) -> Value {
        self.requests.recv_timeout(LIMIT).unwrap()
    }
    fn no_request(&self) {
        assert!(matches!(
            self.requests.try_recv(),
            Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
        drop(self.bridge.take());
        #[cfg(unix)]
        {
            let deadline = Instant::now() + LIMIT;
            while std::fs::remove_dir(self.path.parent().unwrap()).is_err() {
                assert!(Instant::now() < deadline, "owned broker did not clean up");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

struct Mcp {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    pending: HashMap<u64, Value>,
    service: tokio::task::JoinHandle<()>,
}
impl Mcp {
    async fn new(server: NovaServer) -> Self {
        let (client, transport) = tokio::io::duplex(1_000_000);
        let (reader, writer) = tokio::io::split(client);
        let service = tokio::spawn(async move {
            let (reader, writer) = tokio::io::split(transport);
            server
                .serve((reader, writer))
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let mut mcp = Self {
            reader: BufReader::new(reader),
            writer,
            pending: HashMap::new(),
            service,
        };
        mcp.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"canonical-terminal-fixture","version":"1"}}})).await;
        assert!(mcp.receive(1).await["result"]["serverInfo"].is_object());
        mcp.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        mcp
    }
    async fn send(&mut self, value: Value) {
        self.writer
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.writer.flush().await.unwrap();
    }
    async fn tool(&mut self, id: u64, name: &str, arguments: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
    }
    async fn receive(&mut self, id: u64) -> Value {
        if let Some(value) = self.pending.remove(&id) {
            return value;
        }
        loop {
            let mut line = String::new();
            assert!(
                tokio::time::timeout(LIMIT, self.reader.read_line(&mut line))
                    .await
                    .unwrap()
                    .unwrap()
                    > 0
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
            if let Some(id) = value["id"].as_u64() {
                self.pending.insert(id, value);
            }
        }
    }
    async fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.tool(id, name, arguments).await;
        self.receive(id).await
    }
    async fn paired(fixture: &Fixture) -> Self {
        let mut mcp = Self::new(
            NovaServer::new().with_chrome_bridge(fixture.bridge.as_ref().unwrap().clone()),
        )
        .await;
        assert_ne!(
            mcp.call(2, "chrome_pair", json!({})).await["result"]["isError"],
            true
        );
        mcp
    }
}
impl Drop for Mcp {
    fn drop(&mut self) {
        self.service.abort();
    }
}
fn text(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}
fn generation(response: &Value) -> String {
    text(response)
        .split("snapshot_id=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string()
}
fn node(id: &str, name: &str, actions: Value) -> Value {
    json!({"nodeId":id,"role":"button","name":name,"actions":actions,"states":{"enabled":true},
        "bounds":{"x":999,"y":999,"width":99,"height":99}})
}
fn page(nodes: Vec<Value>) -> Value {
    json!({"status":"ok","result":{"snapshotId":"provider-snapshot","coverage":"top_document_and_same_origin_children",
        "truncated":false,"frameCoverage":{"status":"partial","documents":2,"reasons":["cross_origin_ancestry"]},"nodes":nodes}})
}
fn simple_page() -> Value {
    page(vec![
        node("node-1", "重复 🪷", json!(["activate"])),
        node("child:7:child-document:n1", "重复 🪷", json!(["activate"])),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canonical_projection_preserves_modes_filter_unicode_provenance_and_exact_action_ids() {
    let fixture = Fixture::new();
    let mut mcp = Mcp::paired(&fixture).await;
    let mut content = node("content", "Read\n\t\r 🪷", json!(["focus", "set_value"]));
    content["value"] = json!({"kind":"redacted","text":"must-not-appear"});
    let nodes = vec![
        node("node-1", "重复 🪷", json!(["activate"])),
        node("child:7:child-document:n1", "重复 🪷", json!(["activate"])),
        content,
    ];
    fixture.reply(page(nodes.clone()));
    let read = mcp
        .call(3, "ax_read", json!({"target":"paired_page","mode":"all"}))
        .await;
    assert_ne!(read["result"]["isError"], true, "{read}");
    let request = fixture.next();
    assert_eq!(request["args"], json!({"maxNodes":200,"maxChars":30000}));
    let rendered = text(&read);
    for expected in [
        "provider=chrome_extension",
        "scope=page",
        "status=partial",
        "nativeWindowAssociation=unproven",
        "documents=2",
        "cross_origin_ancestry",
        "top_document_id=\"document-7\"",
        "[n1]",
        "[n2]",
        "[n3]",
        "重复 🪷",
        "[REDACTED]",
        "actionable=false",
        "provider_node_id=\"child:7:child-document:n1\"",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}: {rendered}"
        );
    }
    for forbidden in ["bounds=", "mark=", "pid=", "window_id=", "must-not-appear"] {
        assert!(!rendered.contains(forbidden), "{rendered}");
    }
    assert_eq!(
        rendered.lines().count(),
        4,
        "control characters must not forge node lines"
    );
    let snapshot = generation(&read);
    for (id, node_id) in [(4, "missing"), (5, "n3")] {
        assert_eq!(
            mcp.call(
                id,
                "ax_activate",
                json!({"snapshot_id":snapshot,"node_id":node_id})
            )
            .await["result"]["isError"],
            true
        );
    }
    fixture.no_request();
    fixture.reply(json!({"status":"ok","result":{"activated":true,"method":"fixture"}}));
    let activated = mcp
        .call(
            6,
            "ax_activate",
            json!({"snapshot_id":snapshot,"node_id":"n2"}),
        )
        .await;
    assert_ne!(activated["result"]["isError"], true, "{activated}");
    assert!(text(&activated).contains("fixture-receipt"));
    assert_eq!(
        fixture.next()["args"],
        json!({"snapshotId":"provider-snapshot","nodeId":"child:7:child-document:n1"})
    );
    assert_eq!(
        mcp.call(
            7,
            "ax_activate",
            json!({"snapshot_id":snapshot,"node_id":"n1"})
        )
        .await["result"]["isError"],
        true
    );
    fixture.no_request();
    fixture.reply(page(nodes.clone()));
    let interactive = mcp
        .call(
            8,
            "read_ui",
            json!({"target":"paired_page","mode":"interactive","filter":"not-found"}),
        )
        .await;
    fixture.next();
    assert!(!text(&interactive).contains("[n"));
    fixture.reply(json!({"status":"ok","result":{"activated":true}}));
    assert_ne!(
        mcp.call(
            9,
            "ax_activate",
            json!({"snapshot_id":generation(&interactive),"node_id":"n1"})
        )
        .await["result"]["isError"],
        true
    );
    fixture.next(); // display filtering retains actionable tokens, as on native.
    fixture.reply(page(nodes));
    let content = mcp
        .call(
            10,
            "ax_read",
            json!({"target":"paired_page","mode":"content","max":2}),
        )
        .await;
    fixture.next();
    assert!(text(&content).contains("truncated=true"));
    assert!(!text(&content).contains("[n3]"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selector_failures_and_invalid_provider_data_are_explicit_and_bounded() {
    let mut absent = Mcp::new(NovaServer::new()).await;
    let unavailable = absent
        .call(3, "ax_read", json!({"target":"paired_page"}))
        .await;
    assert_eq!(unavailable["result"]["isError"], true);
    assert!(text(&unavailable).contains("status=unavailable"));
    let fixture = Fixture::new();
    let mut mcp =
        Mcp::new(NovaServer::new().with_chrome_bridge(fixture.bridge.as_ref().unwrap().clone()))
            .await;
    for (id, args, expected) in [
        (
            3,
            json!({"target":"paired_page","window":""}),
            "invalid_target",
        ),
        (4, json!({"target":"unknown"}), "unsupported_target"),
        (5, json!({"target":"paired_page"}), "not paired"),
    ] {
        let reply = mcp.call(id, "ax_read", args).await;
        assert_eq!(reply["result"]["isError"], true);
        assert!(text(&reply).contains(expected), "{reply}");
    }
    fixture.no_request();
    mcp.call(6, "chrome_pair", json!({})).await;
    for (id, body) in [
        (
            7,
            page(vec![
                node("same", "A", json!(["activate"])),
                node("same", "B", json!(["activate"])),
            ]),
        ),
        (
            8,
            page(vec![
                json!({"nodeId":"x","role":"button","name":"A","states":{},"actions":["invented"]}),
            ]),
        ),
    ] {
        fixture.reply(body);
        let reply = mcp
            .call(id, "ax_read", json!({"target":"paired_page"}))
            .await;
        fixture.next();
        assert_eq!(reply["result"]["isError"], true);
        assert!(text(&reply).contains("invalid_provider_snapshot"));
    }
    fixture.reply(page(vec![node(
        "huge",
        &"🪷\n\"".repeat(6000),
        json!(["activate"]),
    )]));
    let capped = mcp
        .call(
            9,
            "ax_read",
            json!({"target":"paired_page","max_chars":4096}),
        )
        .await;
    fixture.next();
    assert!(text(&capped).chars().count() <= 4096);
    assert!(text(&capped).contains("partial_reason=character_limit"));
    fixture.reply(
        json!({"status":"error","error":{"code":"fixture_denied","message":"🪷".repeat(9000)}}),
    );
    let failed = mcp
        .call(
            10,
            "ax_read",
            json!({"target":"paired_page","max_chars":4096}),
        )
        .await;
    fixture.next();
    assert_eq!(failed["result"]["isError"], true);
    assert!(
        text(&failed).chars().count() <= 4096 && text(&failed).contains("terminal_truncated=true")
    );
    assert!(text(&failed).contains("fixture-receipt"));
    fixture.no_request();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn superseded_reads_cannot_publish_and_concurrent_activations_dispatch_once() {
    let fixture = Fixture::new();
    let mut mcp = Mcp::paired(&fixture).await;
    let old = fixture.hold();
    mcp.tool(3, "ax_read", json!({"target":"paired_page"}))
        .await;
    fixture.next();
    // A selector error still reserves the newer generation before it returns;
    // its response is a deterministic barrier while the old read is held.
    let newer = mcp
        .call(4, "ax_read", json!({"target":"unsupported"}))
        .await;
    assert_eq!(newer["result"]["isError"], true);
    old.send(simple_page()).unwrap();
    assert!(text(&mcp.receive(3).await).contains("superseded"));
    fixture.reply(simple_page());
    let fresh = mcp
        .call(5, "ax_read", json!({"target":"paired_page"}))
        .await;
    fixture.next();
    let snapshot = generation(&fresh);
    let held = fixture.hold();
    mcp.tool(
        6,
        "ax_activate",
        json!({"snapshot_id":snapshot,"node_id":"n1"}),
    )
    .await;
    assert_eq!(fixture.next()["action"], "activate");
    assert_eq!(
        mcp.call(
            7,
            "ax_activate",
            json!({"snapshot_id":snapshot,"node_id":"n2"})
        )
        .await["result"]["isError"],
        true
    );
    fixture.no_request();
    // New generation invalidation completes while provider I/O is held: the
    // short action gate was released before waiting for the DOM terminal.
    assert_eq!(
        mcp.call(8, "ax_read", json!({"target":"unsupported"}))
            .await["result"]["isError"],
        true
    );
    held.send(json!({"status":"ok","result":{"activated":true}}))
        .unwrap();
    assert_ne!(mcp.receive(6).await["result"]["isError"], true);
    fixture.reply(simple_page());
    let newest = mcp
        .call(9, "ax_read", json!({"target":"paired_page"}))
        .await;
    fixture.next();
    assert_eq!(
        mcp.call(
            10,
            "ax_activate",
            json!({"snapshot_id":snapshot,"node_id":"n1"})
        )
        .await["result"]["isError"],
        true
    );
    fixture.reply(json!({"status":"ok","result":{"activated":true}}));
    assert_ne!(
        mcp.call(
            11,
            "ax_activate",
            json!({"snapshot_id":generation(&newest),"node_id":"n2"})
        )
        .await["result"]["isError"],
        true
    );
    fixture.next(); // stale prior ID did not consume the newer generation.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_errors_ambiguity_and_timeout_consume_without_replay_or_native_fallback() {
    for outcome in ["error", "ambiguous", "timeout"] {
        let fixture = Fixture::new();
        let mut mcp = Mcp::paired(&fixture).await;
        fixture.reply(simple_page());
        let read = mcp
            .call(3, "ax_read", json!({"target":"paired_page"}))
            .await;
        fixture.next();
        let snapshot = generation(&read);
        if outcome == "timeout" {
            fixture.plans.send(Plan::Silence).unwrap();
        } else {
            fixture.reply(json!({"status":outcome,"error":{"code":"no_observed_effect","message":"fixture DOM failure"},"result":{"activated":false}}));
        }
        let failed = mcp
            .call(
                4,
                "ax_activate",
                json!({"snapshot_id":snapshot,"node_id":"n1"}),
            )
            .await;
        assert_eq!(fixture.next()["action"], "activate");
        assert_eq!(failed["result"]["isError"], true, "{failed}");
        assert!(text(&failed).contains("consumed"));
        if outcome != "timeout" {
            assert!(
                text(&failed).contains("no_observed_effect")
                    && text(&failed).contains("fixture-receipt")
            );
        }
        assert_eq!(
            mcp.call(
                5,
                "ax_activate",
                json!({"snapshot_id":snapshot,"node_id":"n1"})
            )
            .await["result"]["isError"],
            true
        );
        fixture.no_request();
        if outcome != "error" {
            let disconnected = mcp
                .call(6, "ax_read", json!({"target":"paired_page"}))
                .await;
            assert_eq!(disconnected["result"]["isError"], true);
            assert!(
                text(&disconnected).contains("not connected"),
                "{disconnected}"
            );
        }
    }
}
