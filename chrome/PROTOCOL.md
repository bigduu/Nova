# Chrome bridge protocol v1

Chrome and the native host exchange Chrome Native Messaging frames (a 4-byte
little-endian length followed by one UTF-8 JSON object). The native host and
Nova.app exchange the same JSON objects as newline-delimited JSON over the
private per-user `chrome.sock`.

All envelopes carry `protocolVersion: 1` and one of these `kind` values:

| Direction | Kind | Purpose |
| --- | --- | --- |
| Extension → app | `hello` | Announces a fresh extension/native connection. |
| App → extension | `request` | Invokes one semantic tool. |
| Extension → app | `result` | Returns one terminal result plus a short-lived receipt. |
| App → extension | `receipt` | Acknowledges the exact result. |
| Either | `event` | Reports revocation or pairing lifecycle state. |

Supported request actions are `status`, `pair`, `release`, `read`, `activate`,
`focus`, `set_value`, and `scroll`.

`pair` does not immediately grant access. It creates a pending request for at
most 30 seconds. A user must confirm the currently active top-level page in the
extension popup. Its result contains the only valid route:

```json
{
  "tabId": 42,
  "documentId": "Chrome-owned-document-token",
  "nonce": "content-script-random-nonce",
  "epoch": 7
}
```

Every operation other than `status` and the initial `pair` must echo that
route. `read` returns a `snapshotId` and semantic nodes. Node actions must echo
the same `snapshotId` and a returned `nodeId`; stale snapshots fail closed.
There is no `(x, y)` action and no coordinate fallback.

Request IDs are single-use for the life of an extension worker. Reuse is
rejected. If the reused ID or a returned result disagrees about its action, the
outcome is `ambiguous` and is never executed. Results have opaque receipts that
expire after 30 seconds. Releasing/revoking a route increments the epoch and
invalidates every outstanding request and receipt from the older epoch.

For `set_value`, plaintext exists only long enough to set the selected DOM
control. Diagnostics and result metadata contain only UTF-8 byte length and a
SHA-256 digest. Password, file, one-time-code, and payment credential controls
are excluded from snapshots and cannot be targeted.
