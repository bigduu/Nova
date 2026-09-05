# Nova

**A Computer Use implementation in Rust.** Nova is a [Model Context
Protocol](https://modelcontextprotocol.io) server that gives an LLM agent
AX-first control of macOS and Windows: semantic UI reads/actions, screenshots,
mouse, keyboard, scrolling, window/app introspection, OCR, and the clipboard —
the "computer use" capability, built natively in Rust rather than wrapping a
Python/JS automation stack.

Built directly on native platform APIs (macOS Accessibility/ScreenCaptureKit/
CoreGraphics and Windows UI Automation/Win32) — a single self-contained binary,
no runtime to install.
Connect it to any MCP client (Claude Desktop, an agent runtime, your own) over
stdio or Streamable HTTP.

## Tools

| Tool | What it does |
| --- | --- |
| `ax_read` | Canonical `ax:read`: read semantic labels, text, values, roles, actions, state, and optional bounds through macOS Accessibility or Windows UIA, without a screenshot. Returns an ephemeral snapshot/node protocol and explicit coverage/status. |
| `read_ui` | Compatibility alias backed by the same `ax_read` traversal and cache generation. |
| `ax_activate` | Activate an exact actionable node from a fresh `ax_read`; rejects stale snapshot IDs and reports `route=ax\|uia\|web_dom\|element_center`. Every attempt consumes its generation before provider dispatch. |
| `screenshot` | Capture the whole display or a single `window=` — use for layout, icons, colors, images, canvas, and visual verification after semantic/OCR paths. |
| `zoom_region` | Magnify a rectangle of the last screenshot at native resolution — reads small targets on surfaces with no Accessibility tree. |
| `ocr` | Recognize on-screen text with Apple Vision on macOS or Windows Media OCR on Windows. `mode=auto` uses Fast first with confidence-based Accurate fallback; `mode=fast\|accurate` forces either policy. An optional strict `roi={x,y,width,height}` re-captures a rectangle from the current image through the native region path. Returns each line with a clickable center. |
| `click_mark` | Compatibility action for the latest numbered mark; prefer generation-safe `ax_activate`. |
| `left_click` / `right_click` / `double_click` / `mouse_move` / `scroll` | Pointer input in the pixel space of the last screenshot. |
| `cursor_position` | Read the cursor in OS-global logical coordinates; it is not converted into the last screenshot's pixel space. |
| `type_text` / `key_combo` | Keyboard input (full Unicode, incl. CJK + emoji). |
| `list_windows` / `list_applications` / `open_application` | Window & app introspection. |
| `inspect_app` | Optional macOS app capability discovery. Accepts an app name/bundle ID, or discovers running Chromium candidates when omitted; no caller-supplied port or permission prompt. |
| `read_clipboard` / `write_clipboard` | Clipboard access. |
| `ax_click` / `ax_set_value` / `ax_focus` | Drive controls by Accessibility role/label. |
| `dump_ax` | Read the raw AX/UIA tree for diagnostics and coverage debugging. |
| `batch_actions` | Run a sequence of input actions in one call. |
| `wait` | Pause for a specified number of seconds. |

## Requirements

- **macOS 14+** for the macOS desktop backend. The release archive is universal
  and runs on Apple Silicon and Intel Macs.
- **Windows x86_64 or ARM64** for the Windows desktop backend. GitHub Releases
  provide a native archive for each architecture.
- On Windows, `ocr` uses installed Windows OCR language packs. Use
  `nova --ocr-langs` to inspect available languages; install the needed pack if
  recognition reports that it is unavailable.
- Building on macOS requires the macOS 15 SDK / Xcode 16+ because of a
  transitive `apple-metal` build dependency; that is a build-time requirement,
  not Nova's minimum macOS runtime version.
- On macOS, **Screen Recording** permission is required for `screenshot`, `ocr`,
  and `list_windows`; **Accessibility** is required for `ax_read`, semantic
  activation, and input.

> macOS grants these permissions to the process it identifies as responsible
> for Nova. The managed `nova mcp` entrypoint and Bamboo plugin use the independent
> Nova.app on macOS. Legacy direct stdio/HTTP can use the host app, terminal, or
> directly launched binary as the permission subject. See
> [Permissions & code signing](#permissions--code-signing-macos).

## Run

```sh
cargo run                      # stdio transport (default)
cargo run -- mcp                # managed MCP: Nova.app on macOS, stdio elsewhere
cargo run -- --http            # Streamable HTTP on 127.0.0.1:3100
cargo run -- --http --addr 127.0.0.1:8080
```

> The Swift runtime that ScreenCaptureKit links is located via an `LC_RPATH`
> baked in by `build.rs`, so no `DYLD_*` environment variable is needed for
> `cargo run`/`cargo test` or the standalone binary.

## Install

The supported public installation paths are a source build and the verified
prebuilt archives on the [`v0.2.1` GitHub
Release](https://github.com/bigduu/Nova/releases/tag/v0.2.1). That release
provides:

| Platform | Archive |
| --- | --- |
| macOS, Apple Silicon + Intel | `nova-v<version>-universal-apple-darwin.tar.gz` |
| Windows x86_64 | `nova-v<version>-x86_64-pc-windows-msvc.zip` |
| Windows ARM64 | `nova-v<version>-aarch64-pc-windows-msvc.zip` |

Download the matching `.sha256` file from the same release and verify the
archive before extracting it. On macOS:

```sh
tar -xzf nova-v*-universal-apple-darwin.tar.gz
xattr -dr com.apple.quarantine ./nova  # only if Gatekeeper blocks the download
sudo install -m 0755 nova /usr/local/bin/nova
```

On Windows, extract the archive for the machine's architecture and invoke
`nova.exe` directly or place its directory on `PATH`. The Windows binaries are
not Authenticode-signed, so SmartScreen may warn on first run.

> `v0.2.1` predates the current AX-first tools (`ax_read`, `read_ui`, and
> `ax_activate`), the managed `mcp` command, and Nova.app. Its archives provide
> the earlier screenshot/mark/input tool set. Build the current source below
> when using this workflow; those features are not in the `v0.2.1` binaries.

To build from source:

```sh
git clone https://github.com/bigduu/Nova.git
cd Nova
cargo build --release --locked
```

The result is `target/release/nova` on macOS or
`target/release/nova.exe` on Windows. The macOS release binary is ad-hoc signed,
not notarized.

### Nova.app development preview

Releases cut from a revision containing the app packaging workflow also attach:

`nova-v<version>-universal-apple-darwin-development-app.zip`

This archive contains a universal `Nova.app` that runs Nova's per-user app
service without a Dock icon. It gives Screen Recording and Accessibility a Nova
application identity instead of making the MCP host (for example, Bodhi) the
permission subject. Install and start it with:

```sh
shasum -a 256 -c nova-v*-universal-apple-darwin-development-app.zip.sha256
unzip nova-v*-universal-apple-darwin-development-app.zip
ditto Nova.app /Applications/Nova.app
open -gj -b com.zenith.nova
```

Install the app independently of Bodhi and the plugin's downloaded CLI. Keep it
at `/Applications/Nova.app` (or `~/Applications/Nova.app`), outside Bodhi.app and
the plugin directory. The plugin still downloads the CLI archive and uses it
only as the connector on macOS; installing/updating the plugin does not install
or update Nova.app. Use a CLI and app built from the same current version.

Configure a stdio MCP client with `nova mcp`, as shown in
[Use it from an MCP client](#use-it-from-an-mcp-client) below. If no app archive
has been published for the current code, build both macOS architectures,
combine them into a universal binary, then assemble the app with
[`package-development-app.sh`](packaging/macos/package-development-app.sh),
which requires a universal binary and the matching Cargo version as arguments.

> [!WARNING]
> The app archive is **DEVELOPMENT ONLY**. It is ad-hoc signed, not Developer ID
> signed, not notarized, and not stapled. Gatekeeper can block it, and replacing
> it with a differently signed build can require granting TCC permissions again.
> The existing universal CLI `.tar.gz` remains the supported artifact consumed
> by Homebrew, npm, and Bamboo; the app `.zip` does not replace it.

## Use it from an MCP client

**Claude Desktop** (or any stdio MCP client) — add Nova to the client's MCP
config. Claude Desktop uses
`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS and
`%APPDATA%\Claude\claude_desktop_config.json` on Windows:

```json
{
  "mcpServers": {
    "nova": { "command": "/absolute/path/to/nova", "args": ["mcp"] }
  }
}
```

`mcp` is the cross-platform managed entrypoint used by the Bamboo plugin.
Windows and Linux headless builds serve ordinary stdio MCP. On macOS it only
connects to the independent Nova.app, launching it through LaunchServices when
needed. Install the app separately, open it once, and grant **Accessibility** to
Nova; **Screen Recording** is needed for capture, OCR, and `list_windows` (the
current preview may request it on app startup). The bundled executable can also
be used as the connector:

```json
{
  "mcpServers": {
    "nova": {
      "command": "/Applications/Nova.app/Contents/MacOS/nova",
      "args": ["mcp"]
    }
  }
}
```

The explicit `--connect` command remains supported and uses the same transport
as macOS `mcp`. It carries MCP bytes over a private per-user Unix socket. The
connector does not call desktop APIs or request macOS permissions;
the app process owns the MCP handlers and TCC responsibility. The socket lives
under `/tmp/nova-app-<uid>/` with a mode-0700 directory, mode-0600 socket, and a
same-UID peer check.

If Nova.app is unavailable, the managed command exits with installation and
reconnection guidance. It never falls back to desktop operations inside the MCP
host. `NOVA_APP_SOCKET` is for isolated development/tests; when set it disables
automatic app launch. Unset it for the normal installed-app setup. Unbundled
`nova` with no arguments still offers the legacy direct stdio mode.

### Chrome DevTools MCP sidecar

For routine Chrome page automation and debugging, Nova can launch the official
[Chrome DevTools MCP](https://github.com/ChromeDevTools/chrome-devtools-mcp)
next to the desktop server. This is a transparent stdio sidecar, not a second
browser implementation inside Nova. It requires npm/`npx`, Node.js
`^20.19.0`, `^22.12.0`, or `>=23`, and current stable Chrome (or newer). Nova
pins the reviewed upstream package to `chrome-devtools-mcp@1.8.0`.

On macOS, a recommended two-server configuration is:

```json
{
  "mcpServers": {
    "nova": {
      "command": "/Applications/Nova.app/Contents/MacOS/nova",
      "args": ["--connect"]
    },
    "nova-chrome-devtools": {
      "command": "/Applications/Nova.app/Contents/MacOS/nova",
      "args": ["chrome-devtools"]
    }
  }
}
```

For a standalone source/release binary, use the same binary path and
`["chrome-devtools"]`. If a GUI client cannot find `npx`, add
`"--npx", "/absolute/path/to/npx"` after the subcommand.

The default launches a new temporary, isolated Chrome profile. Usage
statistics, package update checks, CrUX URL lookups, and sensitive network
headers are disabled/redacted by default. Requests made by attached DevTools
targets can be guarded by repeating `--allowed-url-pattern`, for example:

```json
"args": [
  "chrome-devtools",
  "--allowed-url-pattern", "https://example.com/*",
  "--allowed-url-pattern", "https://*.example.net/*"
]
```

URL allow patterns require Chrome 149+. They apply only to DevTools targets
while the MCP server is attached and are not a complete network sandbox; use
an OS/VM sandbox when full network isolation is required, as described by the
[upstream security policy](https://github.com/ChromeDevTools/chrome-devtools-mcp/security/policy).

To work with an already running signed-in Chrome profile instead, first open
`chrome://inspect/#remote-debugging` in Chrome and enable remote debugging,
then configure:

```json
"args": ["chrome-devtools", "--profile", "existing"]
```

Automatic connection requires Chrome 144+. If several Chrome profiles are
active, Chrome chooses the profile it considers the default; select and verify
the connected pages before acting.

> [!WARNING]
> Existing-profile mode can inspect and control every open window in the
> selected Chrome profile, including authenticated pages. Enable it only for a
> trusted local MCP client, and disable remote debugging when finished.

Use `--enable-webmcp` to expose upstream's experimental WebMCP tools. Nova adds
Chrome's required `--enable-features=WebMCP` launch argument in isolated mode;
for an existing profile, Chrome itself must already have been started with that
feature enabled. WebMCP requires Chrome 150+. `--expose-network-headers` and
`--enable-performance-crux` are explicit privacy opt-ins. The pinned 1.8.0
package does not support a
`--disable-javascript-evaluation` option, so Nova does not advertise or pass it.

The sidecar and Nova's optional [Secure Chrome Bridge](chrome/README.md) serve
different trust models: DevTools MCP is the broad, full-featured choice for
normal browser automation, DOM/network inspection, and performance debugging;
the Secure Chrome Bridge requires explicit per-page pairing and is preferable
when least-privilege page scoping matters. Nova's desktop tools remain the path
for browser chrome, native dialogs, and non-web UI.

Use the absolute path to the extracted release binary or the source-build
output. On Windows, use an escaped executable path such as
`"C:\\absolute\\path\\nova.exe"`. Use the source-build output for the
AX-first workflow below. If its directory is already on `PATH`, the command can
be `"nova"`.

Reconnect/reload the Nova MCP server in the client; Bodhi's main window can stay
open. See
[Permissions & code signing](#permissions--code-signing-macos) for legacy
direct-stdio and development-binary cases.

**HTTP clients** — run Nova as a server and connect over Streamable HTTP:

```sh
nova --http                       # 127.0.0.1:3100/mcp
nova --http --addr 127.0.0.1:8080 # custom loopback port
```

HTTP mode is currently a local transport: it keeps rmcp's default loopback
Host allowlist and does not configure remote-access authentication. Binding all
interfaces is not a supported LAN setup.

**First calls (current source build).** Call `ax_read` (optionally
`ax_read(window="<name>", mode="all")`) for semantic content and controls, then
`ax_activate(snapshot_id, node_id)` on an exact actionable node. Re-run
`ax_read` after the action to verify semantic state. If AX/UIA coverage is
absent or partial, use focused-window `ocr` for rendered text; use
`screenshot(window=...)` / `zoom_region` only when pixels are necessary
(layout, icon, color, image, canvas, or visual verification). All pointer tools
use the pixel space of the most recent screenshot; `cursor_position` instead
reports OS-global logical coordinates.

## Permissions & code signing (macOS)

### Inspect an application's interaction options

Use `inspect_app` when setting up an application or checking which interaction
route is available. It is optional; ordinary native interaction still starts
with `ax_read`.

```json
{"app": "Slack"}
```

The selector accepts a running application's name or bundle identifier. Exact
matches take priority over partial matches. Omit `app` to discover running
Electron, Chromium, and CEF candidates, including applications with no discovered
debugging connection. Names alone do not confirm a runtime: Nova checks known
framework containers and their executable evidence. Unknown or unreadable
bundles remain unknown.

The default result contains application identity, runtime, inspection status,
the currently available native route, and a next step. Nova finds process-owned
local connection candidates internally; callers do not need to find or supply
ports. For diagnostics only, use:

```json
{"app": "com.example.application", "details": true}
```

Detailed output includes bundle/runtime evidence, process start identities,
endpoint provenance, and metadata verification. Nova checks the selected app's
owned listeners, recognized debugging flags, and the exact `DevToolsActivePort`
file only when a `--user-data-dir` flag evidences the profile location. It does
not scan profile contents or return full arguments/environment. Programmatically
enabled ports can be discovered through listener ownership even when a flag is
absent from the OS argument list.

`browser_endpoint_available` means a metadata-only browser handshake succeeded;
it does **not** attach browser tools, grant authorization, or verify the full
Chrome DevTools MCP toolset. Native `ax_read` still uses Accessibility, and the
result reports when that permission is needed. Node inspector endpoints,
incompatible endpoints, stale evidence, and incomplete inspection remain
distinct. No discovered port is not proof that debugging is disabled. Enablement
and whether a particular application can support a restart-based change remain
unknown until verified for that application.

Discovery does not launch, focus, quit, or restart applications, request
permissions, modify bundles/arguments, or open a debugging service. Network
requests stay on verified process-owned loopback sockets: `/json/version`,
`Browser.getVersion`, and `Target.getBrowserContexts` only. There is no page
enumeration, script evaluation, input, or `Browser.close`. HTTP proxies and
redirects are disabled; advertised WebSockets must keep the same owned address
and port. Ownership/start identity is checked before and after probing.

An investigation allows 8 seconds overall, 16 result apps, 32 processes per app,
4 helper generations, 8 endpoint probes per app, and 2 evidenced profiles. Each
metadata probe has a 900 ms deadline; HTTP bodies and WebSocket messages are
limited to 32 KiB, the WebSocket exchange to 128 KiB and 16 frames per reply.
Framework lookup is limited to 64 entries in an app's `Contents/Frameworks`,
plus at most four version directories in each recognized framework. Limits
or unavailable evidence are reported as incomplete, rather than silently
claiming that an application has no debugging support. Concurrent calls receive
a busy result. Windows/Linux return an explicit unsupported result; their
existing native tools are unchanged.

On macOS, resident desktop transports keep the process main run loop active,
so applications launched or quit after the first inspection appear or disappear
without restarting Nova or its MCP host. Relaunching an application triggers
fresh process/start-time and endpoint ownership checks. This inventory refresh
does not require Screen Recording or Accessibility permission. The `mcp` and
`--connect` byte proxies return before this desktop event loop and bootstrap.

The automated tests use fake bundles, process records, and loopback services.
The ignored `own_listener_and_process_start_identity_match` test inspects only
its own process/listener. The ignored `e2e_app_inspection` acceptance test requires
an explicitly prepared app with a `dev.nova.acceptance.*` bundle identifier and
`NOVA_TEST_APP_BUNDLE_ID`; it never defaults to inspecting the user's running
applications.

`cargo test --test e2e_resident_app_inspection` runs a separate macOS regression
whose test binary owns the real process main thread. In one resident process it
seeds discovery, launches a unique temporary AppKit app, checks appearance,
quits it, checks disappearance, and checks a new process identity on relaunch.
It repeats this with an internally allocated random listener that simulates
the narrow CDP handshake; the fixture is not Chromium and is reported as an
unknown runtime. It creates no windows and requests no permissions. Real
Electron/Chromium lifecycle acceptance remains a separate check. The test-only
`--without-main-loop` argument is a negative control that reproduces the old
stale-inventory failure; it is expected to fail.

### Permission ownership

The independent app transport is the preferred permission model: grant
**Screen Recording** and **Accessibility** to `Nova.app`, then use
`nova mcp` (or explicit `nova --connect`). The connector never initializes
CoreGraphics or Accessibility, so Bamboo, Claude Desktop, and terminals no
longer need Nova's desktop permissions.

Keep Nova.app installed independently and unchanged when upgrading Bodhi. The
new Bodhi/plugin connector connects to the same app-owned service, so its own
build/signing identity does not become Nova's permission subject. This is an
architectural guarantee about where desktop calls execute; signed installation
and real TCC upgrade acceptance remain separate release gates. Replacing Nova.app
itself, changing its signature, or an OS permission decision can still require
granting permissions again. The development preview is ad-hoc signed.

After granting Nova permissions in System Settings, retry the tool. If macOS
requires a restart for the change, quit/reopen **Nova.app**, then reconnect only
the **Nova MCP server** in the client. Keep Bodhi's main window open. The
connector does not replay interrupted requests or automatically restore an MCP
session after Nova exits. Do not remove/re-add Bodhi's grants to repair this
managed Nova path.

Two details still matter for direct stdio/HTTP and source-development modes:

**Grant the responsible process for the way Nova is launched.** macOS TCC may
attribute a child process to its responsible parent app. For legacy direct
stdio MCP (an empty argument list), grant Claude Desktop, Bamboo, or the
terminal/IDE that launches Nova. For a directly launched CLI/HTTP process, macOS may instead use the Nova
binary. If granting the expected host does not work, add the installed `nova`
binary (or `target/release/nova`) as a fallback under *System Settings → Privacy
& Security → Screen Recording* and *Accessibility*.

**Keep the identity of whichever process receives the grant stable.** If Nova
itself is the permission subject, `cargo build` produces an ad-hoc,
*linker-signed* binary whose code-signing identity is a content hash
(`nova-<hash>`). It changes every build, so a direct binary grant stops applying.
Sign Nova with a stable self-signed identity when developing in that mode:

```sh
cargo build --release
./scripts/dev-codesign.sh --release   # re-sign after EVERY build
```

The first run creates a `Zenith Nova Code Signing` identity in your login keychain
(click **Always Allow** once if codesign prompts) and signs the binary with a
fixed identifier (`com.zenith.nova`). A direct Nova grant then survives rebuilds
that are re-signed with the same certificate. Host-app grants likewise depend on
the host keeping a stable signing identity.

> **Troubleshooting — `screenshot` fails with a "wedged" / "busy" capture error.**
> All captures (and window enumeration) run in ONE shared per-user daemon
> (`nova --capture-daemon`, flock-elected, socket `/tmp/nova-capture-<uid>-<hash>.sock`),
> because `replayd` keys clients by **executable path** — two same-binary
> ScreenCaptureKit clients evict each other's XPC identity and wedge every new
> stream start. The daemon kills itself if a capture exceeds its 8s watchdog,
> and the client auto-recovers: kill+respawn the daemon, then (second failure)
> SIGKILL all nova capture processes and `killall -9 replayd` — wedges self-heal
> without manual action. If they don't: `nova --selftest` (probes ScreenCaptureKit
> in a sacrificial subprocess, then the daemon path) and read
> `/tmp/nova-capture-worker.log` (step trace) + `/tmp/nova-capture-daemon.log`
> (daemon stderr). Manual remedy = kill the processes holding streams
> (`pkill -f -- --capture-daemon`), NOT replayd: plain `killall replayd` is a
> no-op (replayd ignores SIGTERM), and even `killall -9 replayd` doesn't cure a
> wedge while a stream-holding client survives — it just reconnects and re-wedges
> the fresh replayd.

## Coordinate grounding

A general LLM judging pixel coordinates off a downscaled screenshot is the main
source of mis-clicks — so the primary path avoids pixels entirely.

- **`ax_read` first (no image)** — returns actionable controls and
  non-actionable readable content in deterministic tree order. A successful
  macOS read requires Accessibility but does not contact ScreenCaptureKit.
  `permission_denied` means fix that grant; it is not an instruction to take a
  screenshot.
- **Fresh semantic action** — call `ax_activate` with the returned snapshot and
  node IDs. Native AX/UIA and the browser DOM bridge are tried before a freshly
  revalidated element-center click. Stale generations fail closed; every
  activation attempt consumes its generation before provider dispatch, so read
  again after any result.
- **OCR second** — when coverage is absent/partial and the missing information
  is rendered text, use focused-window OCR and its returned text center with
  `left_click(..., source="ocr_center")`.
- **Screenshot/zoom last** — use pixels for visual-only state or a surface with
  no semantic/text representation; coordinate clicks report
  `route=visual_coordinate`. Screenshot marks and `click_mark` remain available
  for compatibility.

When a screenshot *is* needed, **all click/move/scroll tools work in the pixel space
of the last screenshot** — the server remembers that frame and maps clicks back to
the real screen, so the model just "clicks what it sees":
- `screenshot(window: "<name>")` — capture a single window (substring of its
  title or app name) instead of the whole display. Smaller, sharper image → less
  context and far less downscaling → better precision. Later clicks map into
  that window.
- `zoom_region(x, y, w, h)` — magnify a rectangle of the last screenshot at
  native resolution (capturing only that rectangle). For reading small targets
  on surfaces that expose no Accessibility tree (canvas, games, custom views),
  where coordinates are the only option. A labeled coordinate grid is overlaid
  so the model reads positions straight off the axes.

## Testing

The suite is split into fast, hermetic tests (run by default) and side-effecting
end-to-end tests (opt-in, `#[ignore]`d).

### Default — unit + hermetic integration tests

```sh
cargo test
```

Runs everything that has no side effects and needs no special permission:

- unit tests for coordinate scaling, the key/char keystroke maps, combo parsing,
  batch (de)serialization, and MCP tool registration;
- `tests/e2e_interaction.rs` — screenshot→logical coordinate mapping (via
  `CGDisplay`, no permission needed) and a **non-destructive** clipboard
  round-trip (snapshots and restores the clipboard).

This is what the macOS `test` job runs in CI (see `.github/workflows/ci.yml`;
the workflow also has Windows cross-check and Linux headless jobs).

### End-to-end tests (`#[ignore]`d)

These either post **real input events** (they move the cursor, click, scroll, or
type into the focused window) or require **Screen Recording** permission, so they
are excluded from `cargo test` and must be opted into. Run them on a desktop
session where that's acceptable:

```sh
# all of them
cargo test -- --include-ignored

# or a single one
cargo test --test e2e_input mouse_move_roundtrips_through_cursor_position -- --ignored
```

| Test (file) | What it does | Needs |
| --- | --- | --- |
| `semantic_snapshot_reads…` (`e2e_ax_read`) | Resolves and reads a focused or `NOVA_AX_WINDOW` app through AX/UIA without pixel capture | Accessibility / logged-in UIA desktop |
| `mouse_move_roundtrips…` (`e2e_input`) | Moves the cursor, reads it back via `cursor_position`, asserts the position — restores the cursor | Accessibility |
| `click_events_post…` (`e2e_input`) | Left/right/double click on the empty desktop corner (Esc dismisses the menu) | Accessibility |
| `scroll_events_post…` (`e2e_input`) | Posts vertical scroll events | Accessibility |
| `type_text_posts…` (`e2e_input`) | **Types into the focused window** | Accessibility |
| `open_application_launches…` (`e2e_input`) | Launches/focuses System Settings | — |
| `list_windows_returns…` (`e2e_input`) | Enumerates on-screen windows | Screen Recording |
| `e2e_capture_display_returns_valid_jpeg` (`e2e_screenshot`) | Captures the display, checks the JPEG | Screen Recording |
| `e2e_capture_dims_match_target_dims_contract` (`e2e_screenshot`) | Asserts capture dims match the click-coordinate mapping | Screen Recording |
| `e2e_window_screenshot_produces_view_frame` (`e2e_screenshot`) | Captures a window and validates its view-frame metadata | Screen Recording |
| `ocr_recognizes_text_on_the_display` (`e2e_ocr`) | Runs Apple Vision OCR on a live capture; asserts text + in-bounds line centers | Screen Recording |
| `daemon_*` / `client_*` / `concurrent_*` (`e2e_capture_worker`) | Shared capture daemon: capture, kill→respawn recovery, concurrent clients, clean-error survival | Screen Recording |
| `legacy_pipe_protocol_still_served` (`e2e_worker`) | Old `--capture-worker` pipe protocol, proxied into the daemon | Screen Recording |
| `stdio_server_completes_handshake_and_lists_tools` (`e2e_stdio`) | Exercises the stdio (JSON-RPC) transport end-to-end | — |
| `safari_opens_google_and_nova_reads_the_homepage` (`e2e_safari_google`) | Launches Safari, opens Google, and reads the page through Nova | Network + Screen Recording + Accessibility |

> `mouse_move_roundtrips…` proves the macOS pointer post and cursor read-back
> round-trip using logical coordinates. The non-ignored interaction test covers
> screenshot→logical coordinate arithmetic, while the live screenshot tests
> cover captured-dimension contracts.
>
> Run `e2e_capture_worker` **single-threaded** (`-- --ignored --test-threads=1`):
> the tests share one daemon/socket.

`list_applications_returns_app_bundles` (in `e2e_input`) is **not** ignored — it
only reads Spotlight and is tolerant of a Spotlight-less CI host.

### Lint & format

```sh
cargo fmt --all -- --check
cargo clippy --all-targets
```

## Releasing (maintainers)

A version tag drives everything via `.github/workflows/release.yml`. The
workflow resolves the tag once, verifies it against `Cargo.toml` and the event
commit, and makes every source-building job check out that immutable commit. It
builds and smoke-tests the universal macOS CLI and development-only Nova.app,
creates the Release with those assets, then sequenced jobs attach native Windows
x86_64/ARM64 archives and the Bamboo plugin bundle. The CLI `.tar.gz` name and
checksum outputs stay unchanged for Homebrew, npm, and the Bamboo plugin
manifest.

Run the hermetic release checks before tagging:

```sh
scripts/test-release-workflow.sh
```

The current crate version is already published as `v0.2.1`; bump it before
creating the next release tag. Release tags must be protected from force updates;
the workflow also serializes runs by tag and re-verifies the tag before its first
upload. The Nova.app asset must remain labeled
**DEVELOPMENT ONLY** until all production distribution gates are complete:

- sign nested code and the outer app, in that order, with a Developer ID
  Application identity and the hardened runtime;
- submit the distribution artifact to Apple's notary service and verify the
  accepted ticket;
- staple the ticket to the app and validate it with `codesign` and `spctl`;
- authenticate local MCP and Chrome bridge peers with macOS audit tokens and
  designated code requirements, rather than relying on same-UID sockets alone;
- run the packaged native host and extension against a real Chrome install,
  including pairing, navigation revocation, stale snapshots, and disconnects;
- pin third-party GitHub Actions by full commit SHA before treating the release
  workflow as a production supply-chain boundary;
- smoke-test launch, upgrade, `nova --connect`, Screen Recording, and
  Accessibility grants on clean Apple Silicon and Intel macOS 14+ machines.

Do not describe the ad-hoc-signed app preview as a production-ready macOS app.

## License

[MIT](LICENSE) © bigduu
