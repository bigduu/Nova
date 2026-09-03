# Nova for Chrome (developer preview)

This directory contains the Chrome-specific half of Nova. It is deliberately
isolated from the desktop/OCR server:

- `extension/` is an unpacked Manifest V3 extension.
- `native-host/` is the native messaging host and the reusable Nova.app Unix
  socket bridge.

The bridge never turns DOM actions into screen coordinates. A page must first
be explicitly paired, and every action is addressed to the exact top-level
`tabId` + Chrome `documentId` + page nonce + pairing epoch returned by `pair`.
Navigation, native-host disconnect, extension restart, or explicit release
revokes that route.

## Developer setup (macOS)

1. Build the host:

   ```sh
   cargo build --release --manifest-path chrome/native-host/Cargo.toml
   ```

2. Open `chrome://extensions`, enable Developer mode, choose **Load unpacked**,
   and select `chrome/extension`.
3. Copy the extension ID shown by Chrome, then install the native-host manifest:

   ```sh
   chrome/native-host/install-macos.sh EXTENSION_ID
   ```

4. Keep Nova.app running. The host connects only to
   `/tmp/nova-app-<uid>/chrome.sock` (override with `NOVA_CHROME_SOCKET` for an
   isolated test instance).
5. Ask Nova to run `pair`, open the extension popup within 30 seconds, inspect
   the displayed origin, and click **Pair this page**.

This is a developer workflow. The unpacked extension and ad-hoc/native-host
installation are not a production distribution or a Chrome Web Store release.
Production distribution also requires a signed, notarized host, audit-token /
designated-requirement peer authentication beyond the current same-UID socket,
and packaged end-to-end testing against a real Chrome installation.

## Tests

```sh
npm test --prefix chrome/extension
cargo test --manifest-path chrome/native-host/Cargo.toml
```

The JavaScript tests exercise the state machine and protocol without launching
Chrome. The Rust tests cover native-messaging framing, NDJSON limits, message
validation/redaction, socket-path safety, and peer-authentication helpers.
