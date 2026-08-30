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
| `ocr` | Recognize on-screen text with Apple Vision on macOS or Windows Media OCR on Windows. Returns each text line with a clickable center — read *and* click text on canvas/Electron/game surfaces where marks are empty. |
| `click_mark` | Compatibility action for the latest numbered mark; prefer generation-safe `ax_activate`. |
| `left_click` / `right_click` / `double_click` / `mouse_move` / `scroll` | Pointer input in the pixel space of the last screenshot. |
| `cursor_position` | Read the cursor in OS-global logical coordinates; it is not converted into the last screenshot's pixel space. |
| `type_text` / `key_combo` | Keyboard input (full Unicode, incl. CJK + emoji). |
| `list_windows` / `list_applications` / `open_application` | Window & app introspection. |
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
> for Nova. For stdio/plugin use that is usually the host app or terminal; for a
> directly launched Nova process it can be the binary itself. See
> [Permissions & code signing](#permissions--code-signing-macos).

## Run

```sh
cargo run                      # stdio transport (default)
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
> `ax_activate`). Its archives provide the earlier screenshot/mark/input tool
> set. Build the current source below when using the AX-first workflow in this
> README; do not expect those tool names from the `v0.2.1` binaries.

To build from source:

```sh
git clone https://github.com/bigduu/Nova.git
cd Nova
cargo build --release --locked
```

The result is `target/release/nova` on macOS or
`target/release/nova.exe` on Windows. The macOS release binary is ad-hoc signed,
not notarized.

## Use it from an MCP client

**Claude Desktop** (or any stdio MCP client) — add Nova to the client's MCP
config. Claude Desktop uses
`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS and
`%APPDATA%\Claude\claude_desktop_config.json` on Windows:

```json
{
  "mcpServers": {
    "nova": { "command": "/absolute/path/to/nova" }
  }
}
```

Use the absolute path to the extracted release binary or the source-build
output. On Windows, use an escaped executable path such as
`"C:\\absolute\\path\\nova.exe"`. Use the source-build output for the
AX-first workflow below. If its directory is already on `PATH`, the command can
be `"nova"`.

Restart the client; the Nova tools then appear to the agent. On macOS, grant
**Screen Recording** and **Accessibility** — see
[Permissions & code signing](#permissions--code-signing-macos). For stdio MCP,
grant the responsible host (Claude Desktop, Bamboo, or the terminal) first; add
the Nova binary only if macOS treats it as the permission subject or the host
grant does not take effect.

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

Two non-obvious things about how macOS grants Nova its **Screen Recording** and
**Accessibility** permissions:

**Grant the responsible process for the way Nova is launched.** macOS TCC may
attribute a child process to its responsible parent app. For stdio MCP or the
Bamboo plugin, grant Claude Desktop, Bamboo, or the terminal/IDE that launches
Nova. For a directly launched CLI/HTTP process, macOS may instead use the Nova
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

A version tag drives everything via `.github/workflows/release.yml`: it builds
and smoke-tests the universal macOS binary plus native Windows x86_64 and ARM64
binaries, then attaches the archives and checksums to a GitHub Release. It also
publishes the Bamboo plugin manifest and bundle. The tag version must match
`Cargo.toml`; verify the resulting Release assets before treating the release as
available.

## License

[MIT](LICENSE) © bigduu
