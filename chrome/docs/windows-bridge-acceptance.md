# Windows transport acceptance (#72)

The Windows semantic bridge is opt-in for `nova mcp`. Set the exact
`NOVA_CHROME_EXTENSION_ID` (32 characters, a–p) in the managed process and
native host's environment. `NOVA_CHROME_PIPE` can select an isolated local
`\\.\pipe\nova-chrome-` endpoint; without it the endpoint derives from the
user/logon session. Run both processes in the same ordinary user session.
Missing host identity, malformed configuration, peer denial, and an owned pipe
fail with an explanation. Bare `nova`, unconfigured `nova mcp`, and HTTP
retain their existing transport behavior.

The existing broker owns routes, pairing, receipts, and timeouts. One byte-mode,
non-inheritable kernel pipe stays owned through reconnects. Its explicit
owner/logon-session ACL and both-way kernel peer checks restrict local access;
remote pipe clients are rejected. The host transports existing binary Native
Messaging frames and NDJSON, without desktop-control authority.

On a real Windows runner:

```powershell
cargo build --locked --manifest-path chrome/native-host/Cargo.toml
cargo test --locked --manifest-path chrome/native-host/Cargo.toml
$env:NOVA_TEST_CHROME_HOST = Join-Path $PWD "chrome/native-host/target/debug/nova-chrome-host.exe"
cargo test --locked --test e2e_windows_chrome_bridge --test e2e_managed_mcp
```

Tests use unique endpoints and controlled child environments. They launch the
built host, exercise binary framing and exact-origin denial, perform MCP
status/pair/read/receipt round trips, deny an actually impersonated restricted
token through `CreateFileW`, and cover duplicate ownership, partial/drip-fed
messages, unread writes, unanswered requests, reconnect, and shutdown with host
stdin open. Cancellation drains kernel operations before releasing storage.

This is source transport/protocol acceptance. It does not register a host,
write HKCU, use an installed Chrome profile, prove packaged Chrome discovery,
or complete tracker #23. Registration/status/uninstall and packaged Chrome
smoke remain separate acceptance slices.
