//! App-side broker for Nova's private Chrome semantic bridge.
//!
//! [`ChromeBridge`] is deliberately a small, cloneable command handle. A
//! single background thread owns the Unix listener, the native-host stream,
//! the current exact page route, and the request/receipt state machine. This
//! keeps every MCP session in Nova.app on one serialized authority boundary.

#[cfg(unix)]
mod unix {
    use crate::protocol::{validate_message, ACTIONS, PROTOCOL_VERSION};
    use crate::{configured_socket_path, AppBridgeConnection, AppBridgeListener};
    use anyhow::{anyhow, bail, Context, Result};
    use serde_json::{json, Map, Value};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(12);
    const PAIR_TIMEOUT: Duration = Duration::from_secs(32);
    const SHORT_TIMEOUT: Duration = Duration::from_secs(3);
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    const ROUTED_ACTIONS: &[&str] = &[
        "release",
        "read",
        "activate",
        "focus",
        "set_value",
        "scroll",
    ];

    /// Cloneable command handle for the one app-owned Chrome bridge runtime.
    #[derive(Clone)]
    pub struct ChromeBridge {
        sender: mpsc::Sender<Command>,
        sequence: Arc<AtomicU64>,
    }

    impl std::fmt::Debug for ChromeBridge {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ChromeBridge")
                .field("connected_to_broker", &true)
                .finish_non_exhaustive()
        }
    }

    impl ChromeBridge {
        /// Bind the standard private per-user `chrome.sock` endpoint.
        pub fn bind_default() -> Result<Self> {
            Self::bind(configured_socket_path()?)
        }

        /// Bind a private bridge endpoint and start its serialized broker.
        pub fn bind(path: impl AsRef<Path>) -> Result<Self> {
            let listener = AppBridgeListener::bind(path)?;
            listener.set_nonblocking(true)?;
            let (sender, receiver) = mpsc::channel();
            std::thread::Builder::new()
                .name("nova-chrome-app-bridge".to_string())
                .spawn(move || {
                    if let Err(error) = run_bridge(listener, receiver) {
                        eprintln!("Chrome bridge thread stopped: {error:#}");
                    }
                })
                .context("start Chrome app bridge thread")?;
            Ok(Self {
                sender,
                sequence: Arc::new(AtomicU64::new(1)),
            })
        }

        /// Perform one bounded semantic request. Calls are serialized by the
        /// broker and never carry a caller-provided route; the broker adds only
        /// the exact route returned by the confirmed pairing result.
        pub fn call(&self, action: &str, args: Value, timeout: Option<Duration>) -> Result<Value> {
            validate_tool_args(action, &args)?;
            let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
            if timeout.is_zero() {
                bail!("Chrome bridge timeout must be greater than zero");
            }
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
            // The extension retains a bounded replay ledger across pairing
            // epochs. Include this broker process in the ID so a Nova.app
            // restart cannot accidentally reuse an earlier request identity.
            let request_id = format!("app-{}-{sequence}", std::process::id());
            let (reply, response) = mpsc::sync_channel(1);
            self.sender
                .send(Command {
                    request_id,
                    action: action.to_string(),
                    args,
                    expected_route: None,
                    deadline: Instant::now() + timeout,
                    reply,
                })
                .context("Chrome bridge thread stopped")?;
            // The broker is the sole deadline authority. Waiting for its
            // decision guarantees that a timeout has already dropped the
            // native session (and therefore revoked the extension route)
            // before this caller can observe the error and retry. Every
            // broker socket operation is independently bounded, and dropping
            // the broker receiver disconnects this channel, so this wait
            // cannot outlive a stopped broker thread.
            match response.recv() {
                Ok(result) => result,
                Err(mpsc::RecvError) => bail!("Chrome bridge thread stopped"),
            }
        }

        pub fn status(&self) -> Result<Value> {
            self.call("status", json!({}), Some(SHORT_TIMEOUT))
        }

        pub fn pair(&self) -> Result<Value> {
            self.call("pair", json!({}), Some(PAIR_TIMEOUT))
        }

        pub fn release(&self) -> Result<Value> {
            self.call("release", json!({}), Some(SHORT_TIMEOUT))
        }

        pub fn read(&self, max_nodes: Option<u64>, max_chars: Option<u64>) -> Result<Value> {
            self.call(
                "read",
                json!({ "maxNodes": max_nodes, "maxChars": max_chars }),
                None,
            )
        }

        pub fn activate(&self, snapshot_id: &str, node_id: &str) -> Result<Value> {
            self.call(
                "activate",
                json!({ "snapshotId": snapshot_id, "nodeId": node_id }),
                None,
            )
        }

        pub fn focus(&self, snapshot_id: &str, node_id: &str) -> Result<Value> {
            self.call(
                "focus",
                json!({ "snapshotId": snapshot_id, "nodeId": node_id }),
                None,
            )
        }

        pub fn set_value(&self, snapshot_id: &str, node_id: &str, value: &str) -> Result<Value> {
            self.call(
                "set_value",
                json!({
                    "snapshotId": snapshot_id,
                    "nodeId": node_id,
                    "value": value,
                }),
                None,
            )
        }

        pub fn scroll(
            &self,
            snapshot_id: &str,
            node_id: &str,
            direction: &str,
            amount: &str,
        ) -> Result<Value> {
            self.call(
                "scroll",
                json!({
                    "snapshotId": snapshot_id,
                    "nodeId": node_id,
                    "direction": direction,
                    "amount": amount,
                }),
                None,
            )
        }
    }

    struct Command {
        reply: mpsc::SyncSender<Result<Value>>,
        request_id: String,
        action: String,
        deadline: Instant,
        args: Value,
        expected_route: Option<Value>,
    }

    struct Session {
        host_extension_id: Option<String>,
        connection: AppBridgeConnection,
        route: Option<Value>,
        extension_ready: bool,
    }

    impl Session {
        fn new(connection: AppBridgeConnection) -> Self {
            Self {
                host_extension_id: None,
                connection,
                route: None,
                extension_ready: false,
            }
        }

        /// Apply a message received when no tool request is awaiting a result.
        fn handle_idle_message(&mut self, message: Value) -> Result<()> {
            match message.get("kind").and_then(Value::as_str) {
                Some("host_hello") => {
                    if self.host_extension_id.is_some() || self.extension_ready {
                        bail!("duplicate Chrome native host hello");
                    }
                    let extension_id = message
                        .get("extensionId")
                        .and_then(Value::as_str)
                        .context("native host hello is missing extensionId")?;
                    self.host_extension_id = Some(extension_id.to_string());
                    Ok(())
                }
                Some("hello") => {
                    if self.extension_ready {
                        bail!("duplicate Chrome extension hello");
                    }
                    let extension_id = message
                        .get("extensionId")
                        .and_then(Value::as_str)
                        .context("extension hello is missing extensionId")?;
                    if self.host_extension_id.as_deref() != Some(extension_id) {
                        bail!("native host and extension identities disagree");
                    }
                    self.extension_ready = true;
                    Ok(())
                }
                Some("event") if self.extension_ready => {
                    self.apply_event(&message);
                    Ok(())
                }
                // A terminal result can outlive its MCP caller by a few
                // milliseconds. Acknowledge it so the extension can retire
                // the receipt, but never deliver it to a later command.
                Some("result") if self.extension_ready => self.send_receipt(&message),
                Some("event" | "result") => {
                    bail!("Chrome extension handshake is not ready")
                }
                _ => bail!("Chrome bridge protocol direction is invalid"),
            }
        }

        fn apply_event(&mut self, message: &Value) {
            if matches!(
                message.get("name").and_then(Value::as_str),
                Some("route_revoked" | "pair_expired")
            ) {
                self.route = None;
            }
        }

        fn send_receipt(&mut self, result: &Value) -> Result<()> {
            let receipt = result
                .get("receipt")
                .and_then(Value::as_object)
                .context("result receipt is missing")?;
            let receipt_id = receipt
                .get("receiptId")
                .and_then(Value::as_str)
                .context("result receipt ID is missing")?;
            let request_id = result
                .get("requestId")
                .and_then(Value::as_str)
                .context("result request ID is missing")?;
            let action = result
                .get("action")
                .and_then(Value::as_str)
                .context("result action is missing")?;
            let epoch = result
                .get("epoch")
                .and_then(Value::as_u64)
                .context("result epoch is missing")?;

            // The extension intentionally exposes only the opaque receipt ID
            // and expiry in the result. Bind the acknowledgement to the
            // validated outer terminal envelope; the extension's receipt
            // ledger then checks that exact request/action/epoch identity.
            self.connection.send(&json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "receipt",
                "receiptId": receipt_id,
                "requestId": request_id,
                "action": action,
                "epoch": epoch,
            }))
        }

        fn update_route(&mut self, result: &Value, expected_action: &str) -> Result<()> {
            if expected_action == "release" {
                self.route = None;
                return Ok(());
            }
            if result.get("status").and_then(Value::as_str) != Some("ok") {
                return Ok(());
            }

            match expected_action {
                "pair" => {
                    let route = result
                        .get("result")
                        .and_then(|value| value.get("route"))
                        .context("successful pair result is missing route")?;
                    crate::protocol::validate_route(route, true)?;
                    self.route = Some(route.clone());
                }
                "status" => {
                    let status = result.get("result").and_then(Value::as_object);
                    if !status
                        .and_then(|value| value.get("paired"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        self.route = None;
                    } else if let Some(route) = status.and_then(|value| value.get("route")) {
                        crate::protocol::validate_route(route, true)?;
                        self.route = Some(route.clone());
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ResultDisposition {
        Matched,
        DeliverThenDisconnect,
        RejectThenDisconnect,
    }

    /// Own the listener and native-host session until every [`ChromeBridge`]
    /// handle is dropped. Only one command and one terminal result can be in
    /// flight, so request replay and result ambiguity cannot cross MCP sessions.
    fn run_bridge(listener: AppBridgeListener, receiver: mpsc::Receiver<Command>) -> Result<()> {
        let mut session: Option<Session> = None;
        let mut pending: Option<Command> = None;

        loop {
            if session.is_none() {
                match listener.try_accept() {
                    Ok(Some(connection)) => session = Some(Session::new(connection)),
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("nova-chrome-app-bridge: accept failed: {error:#}");
                    }
                }
            }

            // Pump at most one wire message per tick before inspecting command
            // deadlines. A terminal result already waiting at the deadline is
            // therefore acknowledged and delivered instead of being treated
            // as a late, ambiguous completion.
            if session.is_some() {
                pump_socket(&mut session, &mut pending)?;
            } else {
                std::thread::sleep(POLL_INTERVAL);
            }

            if expire_pending(&mut pending, Instant::now()) {
                // A mutating DOM operation may have completed even though its
                // terminal result never arrived. Drop the entire native
                // session so the extension revokes the paired route before a
                // caller can retry and accidentally execute the action twice.
                // Applying the same rule to reads/status keeps one simple,
                // fail-closed ambiguity boundary for every action.
                session = None;
            }

            if pending.is_none() {
                let command = match receiver.try_recv() {
                    Ok(command) => Some(command),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                };
                if let Some(mut command) = command {
                    if command.deadline <= Instant::now() {
                        fail_pending(command, anyhow!("Chrome bridge request expired"));
                        continue;
                    }
                    let Some(active) = session.as_mut() else {
                        fail_pending(command, anyhow!("Chrome native host is not connected"));
                        continue;
                    };
                    if !active.extension_ready {
                        fail_pending(command, anyhow!("Chrome extension handshake is not ready"));
                        continue;
                    }
                    if ROUTED_ACTIONS.contains(&command.action.as_str()) && active.route.is_none() {
                        fail_pending(
                            command,
                            anyhow!("Chrome is not paired with a live top-level document"),
                        );
                        continue;
                    }

                    let request = match build_request(active, &command) {
                        Ok(request) => request,
                        Err(error) => {
                            fail_pending(command, error);
                            session = None;
                            continue;
                        }
                    };
                    command.expected_route = request.get("route").cloned();
                    if let Err(error) = active.connection.send(&request) {
                        fail_pending(command, error.context("send Chrome semantic request"));
                        session = None;
                        continue;
                    }
                    pending = Some(command);
                }
            }
        }
    }

    fn pump_socket(session: &mut Option<Session>, pending: &mut Option<Command>) -> Result<()> {
        let Some(active) = session.as_mut() else {
            return Ok(());
        };
        match active.connection.wait_readable(POLL_INTERVAL) {
            Ok(false) => return Ok(()),
            Ok(true) => {}
            Err(error) => {
                eprintln!("nova-chrome-app-bridge: rejected message: {error:#}");
                if let Some(command) = pending.take() {
                    fail_pending(command, anyhow!("Chrome bridge protocol violation"));
                }
                *session = None;
                return Ok(());
            }
        }

        let message = match active.connection.receive() {
            Ok(Some(message)) => message,
            Ok(None) => {
                if let Some(command) = pending.take() {
                    fail_pending(command, anyhow!("Chrome native host disconnected"));
                }
                *session = None;
                return Ok(());
            }
            Err(error) => {
                eprintln!("nova-chrome-app-bridge: rejected message: {error:#}");
                if let Some(command) = pending.take() {
                    fail_pending(command, anyhow!("Chrome bridge protocol violation"));
                }
                *session = None;
                return Ok(());
            }
        };

        // AppBridgeConnection validates before returning. Retain the explicit
        // authority-boundary check so future transport changes cannot bypass
        // protocol validation.
        if let Err(error) = validate_message(&message) {
            eprintln!("nova-chrome-app-bridge: rejected message: {error:#}");
            if let Some(command) = pending.take() {
                fail_pending(command, anyhow!("Chrome bridge protocol violation"));
            }
            *session = None;
            return Ok(());
        }

        if message.get("kind").and_then(Value::as_str) == Some("result") {
            let Some(command) = pending.take() else {
                if let Err(error) = active.send_receipt(&message) {
                    eprintln!("nova-chrome-app-bridge: stale result failed: {error:#}");
                }
                return Ok(());
            };

            match classify_result(&message, &command) {
                ResultDisposition::Matched => {
                    let delivered = active
                        .update_route(&message, &command.action)
                        .and_then(|()| active.send_receipt(&message))
                        .map(|()| message);
                    let disconnect = delivered.is_err();
                    let _ = command.reply.send(delivered);
                    if disconnect {
                        // A malformed route update or an unacknowledged
                        // terminal receipt leaves the two sides with
                        // ambiguous authority. Reconnect only after Chrome
                        // has revoked the old epoch.
                        *session = None;
                    }
                }
                ResultDisposition::DeliverThenDisconnect => {
                    active.route = None;
                    let delivered = active.send_receipt(&message).map(|()| message);
                    let _ = command.reply.send(delivered);
                    *session = None;
                }
                ResultDisposition::RejectThenDisconnect => {
                    active.route = None;
                    let _ = active.send_receipt(&message);
                    fail_pending(
                        command,
                        anyhow!("ambiguous Chrome result identity; route was revoked"),
                    );
                    *session = None;
                }
            }
            return Ok(());
        }

        if let Err(error) = active.handle_idle_message(message) {
            eprintln!("nova-chrome-app-bridge: handshake/event failed: {error:#}");
            if let Some(command) = pending.take() {
                fail_pending(command, anyhow!("Chrome bridge handshake failed"));
            }
            *session = None;
        }
        Ok(())
    }

    fn fail_pending(command: Command, error: anyhow::Error) {
        let _ = command.reply.send(Err(error));
    }

    /// Complete an expired request and tell the broker to revoke its native
    /// session. Returning the revocation decision makes the timeout policy
    /// directly regression-testable without a real Unix socket.
    fn expire_pending(pending: &mut Option<Command>, now: Instant) -> bool {
        if !pending
            .as_ref()
            .is_some_and(|command| command.deadline <= now)
        {
            return false;
        }
        fail_pending(
            pending.take().expect("expired pending command exists"),
            anyhow!("Chrome semantic action timed out"),
        );
        true
    }

    fn build_request(session: &Session, command: &Command) -> Result<Value> {
        let mut request = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "kind": "request",
            "requestId": command.request_id,
            "action": command.action,
            "args": command.args,
        });
        if ROUTED_ACTIONS.contains(&command.action.as_str()) {
            request["route"] = session
                .route
                .clone()
                .context("Chrome is not paired with a live top-level document")?;
        }
        validate_message(&request)?;
        Ok(request)
    }

    fn classify_result(result: &Value, command: &Command) -> ResultDisposition {
        let request_matches =
            result.get("requestId").and_then(Value::as_str) == Some(command.request_id.as_str());
        if !request_matches {
            return ResultDisposition::RejectThenDisconnect;
        }
        if result.get("status").and_then(Value::as_str) == Some("ambiguous") {
            return ResultDisposition::DeliverThenDisconnect;
        }
        if result.get("action").and_then(Value::as_str) == Some(command.action.as_str()) {
            if ROUTED_ACTIONS.contains(&command.action.as_str()) && command.action != "release" {
                if result.get("route") != command.expected_route.as_ref() {
                    return ResultDisposition::RejectThenDisconnect;
                }

                // A route-disappearance error legitimately echoes the route
                // that was authorized, but its outer epoch has already
                // advanced. Deliver that useful error once, then disconnect
                // so the app cannot retain or reuse the stale route. A
                // successful completion at another epoch is never credible.
                let expected_epoch = command
                    .expected_route
                    .as_ref()
                    .and_then(|route| route.get("epoch"))
                    .and_then(Value::as_u64);
                let result_epoch = result.get("epoch").and_then(Value::as_u64);
                if result_epoch != expected_epoch {
                    return if result.get("status").and_then(Value::as_str) == Some("error") {
                        ResultDisposition::DeliverThenDisconnect
                    } else {
                        ResultDisposition::RejectThenDisconnect
                    };
                }
            }
            ResultDisposition::Matched
        } else {
            ResultDisposition::RejectThenDisconnect
        }
    }

    fn validate_tool_args(action: &str, args: &Value) -> Result<()> {
        if !ACTIONS.contains(&action) {
            bail!("unsupported Chrome semantic action: {action}");
        }
        let object = args
            .as_object()
            .context("Chrome tool args must be an object")?;
        if object.contains_key("x")
            || object.contains_key("y")
            || object.contains_key("coordinates")
        {
            bail!("coordinate fallback is forbidden for Chrome semantic actions");
        }

        match action {
            "status" | "pair" | "release" => require_only(object, &[])?,
            "read" => {
                require_only(object, &["maxNodes", "maxChars"])?;
                optional_u64(object, "maxNodes")?;
                optional_u64(object, "maxChars")?;
            }
            "activate" | "focus" => {
                require_only(object, &["snapshotId", "nodeId"])?;
                validate_identifier(object, "snapshotId", 160)?;
                validate_identifier(object, "nodeId", 160)?;
            }
            "set_value" => {
                require_only(object, &["snapshotId", "nodeId", "value"])?;
                validate_identifier(object, "snapshotId", 160)?;
                validate_identifier(object, "nodeId", 160)?;
                if object.get("value").and_then(Value::as_str).is_none() {
                    bail!("set_value requires a string value");
                }
            }
            "scroll" => {
                require_only(object, &["snapshotId", "nodeId", "direction", "amount"])?;
                validate_identifier(object, "snapshotId", 160)?;
                validate_identifier(object, "nodeId", 160)?;
                if !matches!(
                    object.get("direction").and_then(Value::as_str),
                    Some("up" | "down" | "left" | "right")
                ) {
                    bail!("scroll direction is invalid");
                }
                if !matches!(
                    object.get("amount").and_then(Value::as_str),
                    Some("line" | "half_page" | "page")
                ) {
                    bail!("scroll amount is invalid");
                }
            }
            _ => unreachable!("supported action was exhaustively matched"),
        }
        Ok(())
    }

    fn require_only(object: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
        if let Some(name) = object.keys().find(|name| !allowed.contains(&name.as_str())) {
            bail!("Chrome tool argument {name:?} is invalid");
        }
        Ok(())
    }

    fn optional_u64(object: &Map<String, Value>, name: &str) -> Result<()> {
        if let Some(value) = object.get(name) {
            if !value.is_null() && value.as_u64().is_none() {
                bail!("Chrome tool argument {name:?} is invalid");
            }
        }
        Ok(())
    }

    fn validate_identifier(object: &Map<String, Value>, name: &str, max: usize) -> Result<()> {
        let value = object
            .get(name)
            .and_then(Value::as_str)
            .with_context(|| format!("{name} is invalid"))?;
        if value.is_empty()
            || value.len() > max
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
            })
        {
            bail!("{name} is invalid");
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tool_args_forbid_coordinate_authority() {
            let error = validate_tool_args(
                "activate",
                &json!({ "snapshotId": "s1", "nodeId": "n1", "x": 12 }),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains("coordinate fallback"), "{error}");
        }

        #[test]
        fn tool_args_validate_exact_node_and_scroll_values() {
            validate_tool_args(
                "activate",
                &json!({ "snapshotId": "snapshot-1", "nodeId": "node:2" }),
            )
            .unwrap();
            validate_tool_args(
                "scroll",
                &json!({
                    "snapshotId": "snapshot-1",
                    "nodeId": "root",
                    "direction": "down",
                    "amount": "half_page",
                }),
            )
            .unwrap();
            assert!(validate_tool_args(
                "scroll",
                &json!({
                    "snapshotId": "snapshot-1",
                    "nodeId": "root",
                    "direction": "diagonal",
                    "amount": "page",
                }),
            )
            .is_err());
        }

        #[test]
        fn mismatched_terminal_identity_revokes_instead_of_delivering() {
            let (reply, _response) = mpsc::sync_channel(1);
            let command = Command {
                reply,
                request_id: "app-1".to_string(),
                action: "status".to_string(),
                deadline: Instant::now() + Duration::from_secs(1),
                args: json!({}),
                expected_route: None,
            };
            let wrong = json!({
                "requestId": "app-2",
                "action": "status",
                "status": "ok",
            });
            assert_eq!(
                classify_result(&wrong, &command),
                ResultDisposition::RejectThenDisconnect
            );
        }

        #[test]
        fn routed_terminal_must_echo_the_exact_authorized_route() {
            let expected_route = json!({
                "tabId": 7,
                "documentId": "document-7",
                "nonce": "page-7",
                "epoch": 3,
            });
            let (reply, _response) = mpsc::sync_channel(1);
            let command = Command {
                reply,
                request_id: "app-7".to_string(),
                action: "activate".to_string(),
                deadline: Instant::now() + Duration::from_secs(1),
                args: json!({ "snapshotId": "snapshot-1", "nodeId": "node-1" }),
                expected_route: Some(expected_route.clone()),
            };
            let matching = json!({
                "requestId": "app-7",
                "action": "activate",
                "status": "ok",
                "epoch": 3,
                "route": expected_route,
            });
            assert_eq!(
                classify_result(&matching, &command),
                ResultDisposition::Matched
            );

            let mismatched = json!({
                "requestId": "app-7",
                "action": "activate",
                "status": "ok",
                "epoch": 3,
                "route": {
                    "tabId": 8,
                    "documentId": "document-8",
                    "nonce": "page-8",
                    "epoch": 3,
                },
            });
            assert_eq!(
                classify_result(&mismatched, &command),
                ResultDisposition::RejectThenDisconnect
            );
        }

        #[test]
        fn advanced_terminal_epoch_clears_a_stale_authorized_route() {
            let expected_route = json!({
                "tabId": 7,
                "documentId": "document-7",
                "nonce": "page-7",
                "epoch": 3,
            });
            let (reply, _response) = mpsc::sync_channel(1);
            let command = Command {
                reply,
                request_id: "app-7".to_string(),
                action: "read".to_string(),
                deadline: Instant::now() + Duration::from_secs(1),
                args: json!({}),
                expected_route: Some(expected_route.clone()),
            };

            let revoked = json!({
                "requestId": "app-7",
                "action": "read",
                "status": "error",
                "epoch": 4,
                "route": expected_route,
            });
            assert_eq!(
                classify_result(&revoked, &command),
                ResultDisposition::DeliverThenDisconnect
            );

            let impossible_success = json!({
                "requestId": "app-7",
                "action": "read",
                "status": "ok",
                "epoch": 4,
                "route": command.expected_route.clone().unwrap(),
            });
            assert_eq!(
                classify_result(&impossible_success, &command),
                ResultDisposition::RejectThenDisconnect
            );
        }

        #[test]
        fn terminal_timeout_requires_native_session_revocation() {
            let now = Instant::now();
            let (reply, response) = mpsc::sync_channel(1);
            let mut pending = Some(Command {
                reply,
                request_id: "app-timeout".to_string(),
                action: "set_value".to_string(),
                deadline: now,
                args: json!({
                    "snapshotId": "snapshot-1",
                    "nodeId": "node-1",
                    "value": "not logged",
                }),
                expected_route: None,
            });

            assert!(expire_pending(&mut pending, now));
            assert!(pending.is_none());
            let error = response
                .recv()
                .expect("timeout result")
                .expect_err("expired request must fail")
                .to_string();
            assert!(error.contains("timed out"), "unexpected error: {error}");
        }
    }
}

#[cfg(unix)]
pub use unix::ChromeBridge;

#[cfg(not(unix))]
mod unsupported {
    use anyhow::{bail, Result};
    use serde_json::Value;
    use std::path::Path;
    use std::time::Duration;

    #[derive(Clone, Debug, Default)]
    pub struct ChromeBridge;

    impl ChromeBridge {
        pub fn bind_default() -> Result<Self> {
            bail!("Nova's Chrome semantic bridge requires a Unix-domain socket")
        }

        pub fn bind(_path: impl AsRef<Path>) -> Result<Self> {
            Self::bind_default()
        }

        pub fn call(
            &self,
            _action: &str,
            _args: Value,
            _timeout: Option<Duration>,
        ) -> Result<Value> {
            bail!("Nova's Chrome semantic bridge is unavailable on this platform")
        }

        pub fn status(&self) -> Result<Value> {
            self.call("status", serde_json::json!({}), None)
        }
        pub fn pair(&self) -> Result<Value> {
            self.call("pair", serde_json::json!({}), None)
        }
        pub fn release(&self) -> Result<Value> {
            self.call("release", serde_json::json!({}), None)
        }
        pub fn read(&self, _max_nodes: Option<u64>, _max_chars: Option<u64>) -> Result<Value> {
            self.call("read", serde_json::json!({}), None)
        }
        pub fn activate(&self, _snapshot_id: &str, _node_id: &str) -> Result<Value> {
            self.call("activate", serde_json::json!({}), None)
        }
        pub fn focus(&self, _snapshot_id: &str, _node_id: &str) -> Result<Value> {
            self.call("focus", serde_json::json!({}), None)
        }
        pub fn set_value(&self, _snapshot_id: &str, _node_id: &str, _value: &str) -> Result<Value> {
            self.call("set_value", serde_json::json!({}), None)
        }
        pub fn scroll(
            &self,
            _snapshot_id: &str,
            _node_id: &str,
            _direction: &str,
            _amount: &str,
        ) -> Result<Value> {
            self.call("scroll", serde_json::json!({}), None)
        }
    }
}

#[cfg(not(unix))]
pub use unsupported::ChromeBridge;
