use crate::framing::MAX_MESSAGE_BYTES;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const PROTOCOL_VERSION: u64 = 1;
pub const ACTIONS: &[&str] = &[
    "status",
    "pair",
    "release",
    "read",
    "activate",
    "focus",
    "set_value",
    "scroll",
];
const ROUTED_ACTIONS: &[&str] = &[
    "release",
    "read",
    "activate",
    "focus",
    "set_value",
    "scroll",
];

pub fn validate_message(value: &Value) -> Result<()> {
    let encoded = serde_json::to_vec(value).context("serialize message for validation")?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        bail!("message exceeds bridge limit");
    }
    let object = value.as_object().context("message must be a JSON object")?;
    if object.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        bail!("unsupported protocolVersion");
    }
    match string_field(object, "kind")? {
        "host_hello" => validate_host_hello(object),
        "hello" => validate_hello(object),
        "request" => validate_request(object),
        "result" => validate_result(object),
        "receipt" => validate_receipt(object),
        "event" => validate_event(object),
        _ => bail!("unsupported message kind"),
    }
}

fn validate_host_hello(object: &Map<String, Value>) -> Result<()> {
    if string_field(object, "role")? != "chrome_native_host" {
        bail!("invalid host role");
    }
    validate_extension_id(string_field(object, "extensionId")?)?;
    if object.get("pid").and_then(Value::as_u64).is_none() {
        bail!("host pid is missing");
    }
    Ok(())
}

fn validate_hello(object: &Map<String, Value>) -> Result<()> {
    if string_field(object, "role")? != "chrome_extension" {
        bail!("invalid extension role");
    }
    validate_extension_id(string_field(object, "extensionId")?)
}

fn validate_request(object: &Map<String, Value>) -> Result<()> {
    validate_id(string_field(object, "requestId")?, 160, id_char)?;
    let action = string_field(object, "action")?;
    if !ACTIONS.contains(&action) {
        bail!("unknown request action");
    }
    if ROUTED_ACTIONS.contains(&action) {
        validate_route(
            object
                .get("route")
                .context("routed action is missing route")?,
            true,
        )?;
    } else if let Some(route) = object.get("route") {
        if !route.is_null() {
            validate_route(route, true)?;
        }
    }
    if let Some(args) = object.get("args") {
        if !args.is_object() {
            bail!("request args must be an object");
        }
    }
    Ok(())
}

fn validate_result(object: &Map<String, Value>) -> Result<()> {
    validate_id(string_field(object, "requestId")?, 160, id_char)?;
    let action = string_field(object, "action")?;
    if !ACTIONS.contains(&action) {
        bail!("unknown result action");
    }
    if !matches!(
        string_field(object, "status")?,
        "ok" | "error" | "ambiguous"
    ) {
        bail!("invalid result status");
    }
    positive_integer(object, "epoch")?;
    if let Some(route) = object.get("route") {
        validate_route(route, true)?;
    }
    let receipt = object
        .get("receipt")
        .and_then(Value::as_object)
        .context("result receipt is missing")?;
    validate_id(string_field(receipt, "receiptId")?, 160, id_char)?;
    positive_integer(receipt, "expiresAt")?;
    Ok(())
}

fn validate_receipt(object: &Map<String, Value>) -> Result<()> {
    validate_id(string_field(object, "receiptId")?, 160, id_char)?;
    validate_id(string_field(object, "requestId")?, 160, id_char)?;
    if !ACTIONS.contains(&string_field(object, "action")?) {
        bail!("unknown receipt action");
    }
    positive_integer(object, "epoch")?;
    Ok(())
}

fn validate_event(object: &Map<String, Value>) -> Result<()> {
    validate_id(string_field(object, "name")?, 128, id_char)?;
    positive_integer(object, "epoch")
}

pub fn validate_route(value: &Value, require_epoch: bool) -> Result<()> {
    let route = value.as_object().context("route must be an object")?;
    if route.get("tabId").and_then(Value::as_u64).is_none() {
        bail!("route tabId is invalid");
    }
    validate_id(string_field(route, "documentId")?, 256, id_char)?;
    validate_id(string_field(route, "nonce")?, 128, id_char)?;
    if require_epoch {
        positive_integer(route, "epoch")?;
    }
    Ok(())
}

pub fn host_hello(extension_id: &str) -> Result<Value> {
    validate_extension_origin(&format!("chrome-extension://{extension_id}/"))?;
    Ok(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "kind": "host_hello",
        "role": "chrome_native_host",
        "extensionId": extension_id,
        "pid": std::process::id(),
    }))
}

pub fn validate_extension_origin(origin: &str) -> Result<String> {
    let Some(id) = origin
        .strip_prefix("chrome-extension://")
        .and_then(|rest| rest.strip_suffix('/'))
    else {
        bail!("native host origin is not a Chrome extension");
    };
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        bail!("Chrome extension ID is invalid");
    }
    if let Ok(expected) = std::env::var("NOVA_CHROME_EXTENSION_ID") {
        if expected != id {
            bail!("Chrome extension origin does not match NOVA_CHROME_EXTENSION_ID");
        }
    }
    Ok(id.to_owned())
}

/// Return bounded diagnostics without ever retaining a set_value plaintext.
pub fn redacted_diagnostic(value: &Value) -> Result<Value> {
    validate_message(value)?;
    let mut diagnostic = json!({
        "kind": value.get("kind"),
        "requestId": value.get("requestId"),
        "action": value.get("action"),
    });
    if value.get("kind").and_then(Value::as_str) == Some("request")
        && value.get("action").and_then(Value::as_str) == Some("set_value")
    {
        if let Some(text) = value
            .get("args")
            .and_then(|args| args.get("value"))
            .and_then(Value::as_str)
        {
            let digest = Sha256::digest(text.as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            diagnostic["valueUtf8Bytes"] = json!(text.len());
            diagnostic["valueSha256"] = json!(hex);
        }
    }
    Ok(diagnostic)
}

fn string_field<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} must be a string"))
}

fn positive_integer(object: &Map<String, Value>, key: &str) -> Result<()> {
    if object.get(key).and_then(Value::as_u64).unwrap_or(0) < 1 {
        bail!("{key} must be a positive integer");
    }
    Ok(())
}

fn validate_id(value: &str, max: usize, allowed: fn(u8) -> bool) -> Result<()> {
    if value.is_empty() || value.len() > max || !value.bytes().all(allowed) {
        bail!("invalid opaque identifier");
    }
    Ok(())
}

fn validate_extension_id(value: &str) -> Result<()> {
    if value.len() != 32 || !value.bytes().all(extension_id_char) {
        bail!("invalid Chrome extension ID");
    }
    Ok(())
}

fn id_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
}

fn extension_id_char(byte: u8) -> bool {
    (b'a'..=b'p').contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EXTENSION_ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn route() -> Value {
        json!({
            "tabId": 17,
            "documentId": "document:main-1",
            "nonce": "nonce_1",
            "epoch": 3,
        })
    }

    fn receipt() -> Value {
        json!({
            "receiptId": "receipt-1",
            "expiresAt": 10_000,
        })
    }

    #[test]
    fn validates_every_protocol_envelope_kind() {
        let messages = [
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "host_hello",
                "role": "chrome_native_host",
                "extensionId": EXTENSION_ID,
                "pid": 42,
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "hello",
                "role": "chrome_extension",
                "extensionId": EXTENSION_ID,
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "request",
                "requestId": "app-42-1",
                "action": "status",
                "args": {},
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "request",
                "requestId": "app-42-2",
                "action": "read",
                "route": route(),
                "args": {"maxNodes": 100},
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "result",
                "requestId": "app-42-2",
                "action": "read",
                "status": "ok",
                "epoch": 3,
                "route": route(),
                "receipt": receipt(),
                "result": {"snapshotId": "snapshot-1", "nodes": []},
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "receipt",
                "receiptId": "receipt-1",
                "requestId": "app-42-2",
                "action": "read",
                "epoch": 3,
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "event",
                "name": "route_revoked",
                "epoch": 4,
            }),
        ];

        for message in messages {
            validate_message(&message).unwrap_or_else(|error| {
                panic!("valid {} envelope was rejected: {error:#}", message["kind"])
            });
        }
    }

    #[test]
    fn rejects_bad_handshakes_and_unknown_message_kinds() {
        let invalid = [
            json!({
                "protocolVersion": 2,
                "kind": "hello",
                "role": "chrome_extension",
                "extensionId": EXTENSION_ID,
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "host_hello",
                "role": "chrome_extension",
                "extensionId": EXTENSION_ID,
                "pid": 1,
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "hello",
                "role": "chrome_extension",
                "extensionId": "not-a-valid-extension-id",
            }),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "kind": "surprise",
            }),
        ];

        for message in invalid {
            assert!(validate_message(&message).is_err(), "accepted {message}");
        }
    }

    #[test]
    fn routed_requests_require_a_strict_live_route() {
        let base = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "kind": "request",
            "requestId": "app-42-3",
            "action": "activate",
            "args": {"snapshotId": "snapshot-1", "nodeId": "node-1"},
        });
        let error = validate_message(&base).unwrap_err().to_string();
        assert!(error.contains("missing route"), "{error}");

        let mut invalid_epoch = base.clone();
        invalid_epoch["route"] = route();
        invalid_epoch["route"]["epoch"] = json!(0);
        let error = validate_message(&invalid_epoch).unwrap_err().to_string();
        assert!(error.contains("positive integer"), "{error}");

        let mut invalid_identifier = base.clone();
        invalid_identifier["route"] = route();
        invalid_identifier["route"]["nonce"] = json!("contains spaces");
        assert!(validate_message(&invalid_identifier).is_err());

        let mut valid = base;
        valid["route"] = route();
        validate_message(&valid).unwrap();

        let mut no_epoch = route();
        no_epoch.as_object_mut().unwrap().remove("epoch");
        validate_route(&no_epoch, false).unwrap();
        assert!(validate_route(&no_epoch, true).is_err());
    }

    #[test]
    fn terminal_results_and_receipts_are_fail_closed() {
        let valid_result = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "kind": "result",
            "requestId": "app-42-4",
            "action": "pair",
            "status": "ok",
            "epoch": 1,
            "receipt": receipt(),
        });
        validate_message(&valid_result).unwrap();

        let mut missing_receipt = valid_result.clone();
        missing_receipt.as_object_mut().unwrap().remove("receipt");
        let error = validate_message(&missing_receipt).unwrap_err().to_string();
        assert!(error.contains("receipt is missing"), "{error}");

        let mut bad_status = valid_result.clone();
        bad_status["status"] = json!("done");
        assert!(validate_message(&bad_status).is_err());

        let mut expired_receipt = valid_result.clone();
        expired_receipt["receipt"]["expiresAt"] = json!(0);
        assert!(validate_message(&expired_receipt).is_err());

        let receipt_message = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "kind": "receipt",
            "receiptId": "receipt-1",
            "requestId": "bad request id",
            "action": "pair",
            "epoch": 1,
        });
        assert!(validate_message(&receipt_message).is_err());
    }

    #[test]
    fn redacts_set_value_plaintext_to_bounded_hash_diagnostics() {
        let request = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "kind": "request",
            "requestId": "app-42-5",
            "action": "set_value",
            "route": route(),
            "args": {
                "snapshotId": "snapshot-1",
                "nodeId": "node-1",
                "value": "secret",
            },
        });

        let diagnostic = redacted_diagnostic(&request).unwrap();
        assert_eq!(diagnostic["kind"], "request");
        assert_eq!(diagnostic["requestId"], "app-42-5");
        assert_eq!(diagnostic["action"], "set_value");
        assert_eq!(diagnostic["valueUtf8Bytes"], 6);
        assert_eq!(
            diagnostic["valueSha256"],
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
        );
        assert!(!diagnostic.to_string().contains("secret"));
        assert!(diagnostic.get("args").is_none());
        assert!(diagnostic.get("value").is_none());
    }

    #[test]
    fn validation_rejects_serialized_messages_over_the_bridge_limit() {
        let message = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "kind": "event",
            "name": "route_revoked",
            "epoch": 1,
            "padding": "x".repeat(MAX_MESSAGE_BYTES),
        });
        let error = validate_message(&message).unwrap_err().to_string();
        assert!(error.contains("exceeds bridge limit"), "{error}");
    }

    #[test]
    fn rejects_non_extension_origins_before_any_environment_policy() {
        for origin in [
            "https://example.com/",
            "chrome-extension://too-short/",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/nope",
        ] {
            assert!(
                validate_extension_origin(origin).is_err(),
                "accepted {origin}"
            );
        }
    }
}
