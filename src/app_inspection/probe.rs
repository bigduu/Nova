//! Narrow metadata probe only: GET /json/version, Browser.getVersion, and
//! Target.getBrowserContexts. No target/page enumeration, attach, or evaluation.

use super::Candidate;
use reqwest::{redirect::Policy, Client, Url};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::{client::client_with_config, protocol::WebSocketConfig, Message};

const MAX_BODY: usize = 32 * 1024;
const MAX_WIRE: usize = 128 * 1024;
const PROBE_BUDGET: Duration = Duration::from_millis(900);

pub(super) struct Verification {
    pub status: &'static str,
    pub protocol_version: Option<String>,
    pub product: Option<String>,
}

impl Verification {
    fn status(status: &'static str) -> Self {
        Self {
            status,
            protocol_version: None,
            product: None,
        }
    }
}

/// The advertised WebSocket may not redirect discovery to another process,
/// even on loopback. Its port and IP family/address must match the owned socket.
fn websocket_url(raw: &str, address: SocketAddr) -> Option<Url> {
    let mut url = Url::parse(raw).ok()?;
    if url.scheme() != "ws"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default()? != address.port()
    {
        return None;
    }
    let host = url.host_str()?.trim_matches(['[', ']']);
    if host != "localhost" && host.parse::<IpAddr>().ok()? != address.ip() {
        return None;
    }
    if !address.ip().is_loopback() {
        return None;
    }
    // Never resolve localhost through DNS, and never inherit a proxy.
    url.set_ip_host(address.ip()).ok()?;
    Some(url)
}

pub(super) fn verify(candidate: &Candidate, total_deadline: Instant) -> Verification {
    let deadline = total_deadline.min(Instant::now() + PROBE_BUDGET);
    let result = verify_inner(candidate, deadline);
    match result {
        Ok(result) => result,
        Err(_) if Instant::now() >= deadline => Verification::status("timed_out"),
        Err(_) => Verification::status("incompatible_endpoint"),
    }
}

fn verify_inner(candidate: &Candidate, deadline: Instant) -> anyhow::Result<Verification> {
    anyhow::ensure!(
        candidate.address.ip().is_loopback(),
        "non-loopback candidate"
    );
    let version = metadata(candidate.address, deadline)?;
    let product = bounded_string(&version, "Browser").unwrap_or_default();
    if product.to_ascii_lowercase().starts_with("node.js/")
        || product.to_ascii_lowercase().starts_with("node/")
    {
        return Ok(Verification::status("node_inspector_only"));
    }
    let raw = version
        .get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 2048)
        .ok_or_else(|| anyhow::anyhow!("missing browser endpoint"))?;
    let url = websocket_url(raw, candidate.address)
        .ok_or_else(|| anyhow::anyhow!("unowned advertised endpoint"))?;
    anyhow::ensure!(
        url.path().starts_with("/devtools/browser/"),
        "not a browser endpoint"
    );
    if let Some(path) = &candidate.expected_path {
        if path != url.path() {
            return Ok(Verification::status("stale_evidence"));
        }
    }
    let stream = TcpStream::connect_timeout(&candidate.address, remaining(deadline)?)?;
    let stream = DeadlineStream {
        stream,
        deadline,
        remaining_bytes: MAX_WIRE,
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_BODY))
        .max_frame_size(Some(MAX_BODY));
    let (mut socket, _) = client_with_config(url.as_str(), stream, Some(config))?;
    let mut results = Vec::new();
    for (id, method) in [(1, "Browser.getVersion"), (2, "Target.getBrowserContexts")] {
        socket.send(Message::Text(
            json!({"id": id, "method": method}).to_string().into(),
        ))?;
        let mut result = None;
        // Events/pings cannot keep a discovery alive indefinitely.
        for _ in 0..16 {
            remaining(deadline)?;
            if let Message::Text(text) = socket.read()? {
                let message: Value = serde_json::from_str(&text)?;
                if message.get("id") == Some(&json!(id)) {
                    anyhow::ensure!(message.get("error").is_none(), "browser method unsupported");
                    result = message.get("result").cloned();
                    break;
                }
            }
        }
        results.push(result.ok_or_else(|| anyhow::anyhow!("missing browser reply"))?);
    }
    let product =
        bounded_string(&results[0], "product").ok_or_else(|| anyhow::anyhow!("missing product"))?;
    anyhow::ensure!(
        !product.to_ascii_lowercase().starts_with("node"),
        "Node is not a browser"
    );
    let protocol = bounded_string(&results[0], "protocolVersion")
        .ok_or_else(|| anyhow::anyhow!("missing protocol"))?;
    anyhow::ensure!(
        results[1]
            .get("browserContextIds")
            .is_some_and(Value::is_array),
        "not browser-level CDP"
    );
    // Dropping the socket ends discovery without Browser.close/target mutation.
    Ok(Verification {
        status: "browser_handshake_verified",
        protocol_version: Some(protocol),
        product: Some(product),
    })
}

/// A single async timeout covers headers and every body chunk. Per-read socket
/// timeouts alone let a peer extend discovery by continuously trickling bytes.
fn metadata(address: SocketAddr, deadline: Instant) -> anyhow::Result<Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let request = async {
            let client = Client::builder()
                .no_proxy()
                .redirect(Policy::none())
                .timeout(remaining(deadline)?)
                .connect_timeout(remaining(deadline)?)
                .http1_only()
                .build()?;
            let mut response = client
                .get(format!("http://{address}/json/version"))
                .send()
                .await?;
            anyhow::ensure!(
                response.status().as_u16() == 200,
                "metadata status rejected"
            );
            anyhow::ensure!(
                response
                    .content_length()
                    .is_none_or(|n| n <= MAX_BODY as u64),
                "metadata too large"
            );
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                anyhow::ensure!(body.len() + chunk.len() <= MAX_BODY, "metadata too large");
                body.extend_from_slice(&chunk);
            }
            Ok::<Value, anyhow::Error>(serde_json::from_slice(&body)?)
        };
        tokio::time::timeout(remaining(deadline)?, request).await?
    })
}

fn bounded_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)?
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|n| !n.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "discovery deadline"))
}

/// Apply the ABSOLUTE deadline on every underlying read/write, including
/// tungstenite's internal fragmented-frame/handshake loops and slow trickles.
#[derive(Debug)]
struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
    remaining_bytes: usize,
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining_bytes == 0 {
            return Err(io::Error::other("probe byte limit"));
        }
        self.stream
            .set_read_timeout(Some(remaining(self.deadline)?))?;
        let length = buffer.len().min(self.remaining_bytes);
        let count = self.stream.read(&mut buffer[..length])?;
        self.remaining_bytes -= count;
        Ok(count)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining(self.deadline)?))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertised_endpoints_cannot_escape_the_owned_socket() {
        let address = "127.0.0.1:4567".parse().unwrap();
        for url in [
            "ws://example.com:4567/devtools/browser/x",
            "ws://127.0.0.1:4568/devtools/browser/x",
            "ws://127.0.0.2:4567/devtools/browser/x",
            "ws://user@127.0.0.1:4567/devtools/browser/x",
            "wss://127.0.0.1:4567/devtools/browser/x",
            "ws://127.0.0.1:4567/devtools/browser/x?secret=yes",
        ] {
            assert!(websocket_url(url, address).is_none(), "{url}");
        }
        let v6 = "[::1]:4567".parse().unwrap();
        for raw in [
            "ws://[::1]:4567/devtools/browser/x",
            "ws://localhost:4567/devtools/browser/x",
        ] {
            assert_eq!(websocket_url(raw, v6).unwrap().host_str(), Some("[::1]"));
        }
        assert_eq!(
            websocket_url("ws://localhost:4567/devtools/browser/x", address)
                .unwrap()
                .host_str(),
            Some("127.0.0.1")
        );
    }
}
