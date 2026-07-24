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
| `ocr` | Recognize on-screen text via Apple Vision (on-device, no model files). Returns each text line with a clickable center — read *and* click text on canvas/Electron/game surfaces where marks are empty. CJK + Latin. |
| `click_mark` | Compatibility action for the latest numbered mark; prefer generation-safe `ax_activate`. |
| `left_click` / `right_click` / `double_click` / `mouse_move` / `scroll` / `cursor_position` | Pointer input and cursor query, in the pixel space of the last screenshot. |
| `type_text` / `key_combo` | Keyboard input (full Unicode, incl. CJK + emoji). |
| `list_windows` / `list_applications` / `open_application` | Window & app introspection. |
| `read_clipboard` / `write_clipboard` | Clipboard access. |
| `ax_click` / `ax_set_value` / `ax_focus` / `dump_ax` | Drive controls by Accessibility role/label. |
| `batch_actions` | Run a sequence of input actions in one call. |
| `wait` | Pause for a specified number of seconds. |

## Requirements

- **macOS 15+** (a transitive dependency, `apple-metal`, needs the macOS 15 SDK / Xcode 16+).
- **Screen Recording** permission — for `screenshot` / `ocr` / `list_windows`.
- **Accessibility** permission — for `ax_read`, semantic activation, and input.

> Both are granted to the **nova binary itself**, not the app that launches it —
> see [Permissions & code signing](#permissions--code-signing-macos). On a dev
> build you must also sign nova, or the grant breaks on every rebuild.

## Run

```sh
cargo run                      # stdio transport (default)
cargo run -- --http            # Streamable HTTP on 127.0.0.1:3100
cargo run -- --http --addr 0.0.0.0:8080
```

> The Swift runtime that ScreenCaptureKit links is located via an `LC_RPATH`
> baked in by `build.rs`, so no `DYLD_*` environment variable is needed for
> `cargo run`/`cargo test` or the standalone binary.

## Install

macOS 14+ (Apple Silicon or Intel — the release is a universal binary). Pick one:

```sh
# Homebrew (recommended) — puts `nova` on your PATH
brew install bigduu/tap/nova

# npx — no install; great for MCP configs
npx -y @bigduu/nova --version

# Prebuilt tarball — download, unquarantine, drop on PATH
curl -fsSLO https://github.com/bigduu/Nova/releases/latest/download/nova-vX.Y.Z-universal-apple-darwin.tar.gz
tar xzf nova-*.tar.gz
xattr -dr com.apple.quarantine ./nova   # only needed for the raw download
sudo mv nova /usr/local/bin/

# From source
cargo build --release      # produces target/release/nova
```

> The release binary is **ad-hoc signed**, not notarized. Homebrew and npm
> install it without a Gatekeeper prompt; only the raw tarball download needs
> the one-time `xattr -dr com.apple.quarantine` above.

## Use it from an MCP client

**Claude Desktop** (or any stdio MCP client) — add Nova to the client's MCP
config. For Claude Desktop that's
`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "nova": { "command": "npx", "args": ["-y", "@bigduu/nova"] }
  }
}
```

With `brew install` (or a binary on PATH) the command is simply `"nova"`; from
source, use the absolute path to `target/release/nova`.

Restart the client; the Nova tools then appear to the agent. Grant **Screen
Recording** and **Accessibility** — see
[Permissions & code signing](#permissions--code-signing-macos) (as a subprocess,
the responsible process is usually the **host app or your terminal**, so grant
that, not only the binary).

**HTTP clients** — run Nova as a server and connect over Streamable HTTP:

```sh
nova --http                       # 127.0.0.1:3100/mcp
nova --http --addr 0.0.0.0:8080   # reachable on the LAN
```

**First calls.** Call `ax_read` (optionally
`ax_read(window="<name>", mode="all")`) for semantic content and controls, then
`ax_activate(snapshot_id, node_id)` on an exact actionable node. Re-run
`ax_read` after the action to verify semantic state. If AX/UIA coverage is
absent or partial, use focused-window `ocr` for rendered text; use
`screenshot(window=...)` / `zoom_region` only when pixels are necessary
(layout, icon, color, image, canvas, or visual verification). All pointer tools
use the pixel space of the most recent screenshot.

## Permissions & code signing (macOS)

Two non-obvious things about how macOS grants Nova its **Screen Recording** and
**Accessibility** permissions:

**Nova is its own permission subject — grant the *nova binary*, not the launcher.**
As a standalone CLI (not inside an `.app` bundle), Nova does **not** inherit the
Screen Recording grant of its parent process (your terminal, Claude Desktop, or a
host app). Add the binary itself: *System Settings → Privacy & Security → Screen
Recording* (and *Accessibility*) → `+` → ⌘⇧G → enter the path to
`target/release/nova`, then enable it. A headless subprocess can't trigger the
permission prompt, so adding it by path is the reliable way.

**Sign it, or the grant breaks on every rebuild.** `cargo build` produces an
ad-hoc, *linker-signed* binary whose code-signing identity is a content hash
(`nova-<hash>`) — it changes every build, so macOS treats each rebuild as a brand-
new app and the grant stops applying. Sign with a stable self-signed identity so
the grant persists:

```sh
cargo build --release
./scripts/dev-codesign.sh --release   # re-sign after EVERY build
```

The first run creates a `Zenith Nova Code Signing` identity in your login keychain
(click **Always Allow** once if codesign prompts) and signs the binary with a
fixed identifier (`com.zenith.nova`). Grant the two permissions once (above);
later rebuilds, re-signed with the same cert, keep the grant.

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

This is what CI runs (see `.github/workflows/ci.yml`, macOS runner).

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
| `ocr_recognizes_text_on_the_display` (`e2e_ocr`) | Runs Apple Vision OCR on a live capture; asserts text + in-bounds line centers | Screen Recording |
| `daemon_*` / `client_*` / `concurrent_*` (`e2e_capture_worker`) | Shared capture daemon: capture, kill→respawn recovery, concurrent clients, clean-error survival | Screen Recording |
| `legacy_pipe_protocol_still_served` (`e2e_worker`) | Old `--capture-worker` pipe protocol, proxied into the daemon | Screen Recording |
| `stdio_protocol` (`e2e_stdio`) | Exercises the stdio (JSON-RPC) transport end-to-end | — |
| `safari_google_search` (`e2e_safari_google`) | Launches Safari, searches Google, asserts results — full browser automation flow | Accessibility |

> `mouse_move_roundtrips…` is the strongest input check: it proves events
> actually reach the window server *and* that the screenshot→logical coordinate
> conversion is correct, all click paths share the same posting mechanism.
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
the universal binary (arm64 + x86_64, ad-hoc signed), smoke-tests it, and
attaches it to a GitHub Release. When their secrets are present it also updates
the Homebrew tap and publishes the npm wrapper; without them those steps skip,
so the GitHub Release still ships.

```sh
# bump the version, then tag it (the tag must match Cargo.toml)
git tag v0.1.0
git push origin v0.1.0
```

One-time setup for the optional channels:

- **Homebrew** — `gh repo create bigduu/homebrew-tap --public`, then add a
  fine-grained PAT with **Contents: write** on that repo as the nova-repo secret
  `HOMEBREW_TAP_TOKEN`. (`packaging/homebrew/nova.rb` is the formula template.)
- **npm** — publish under the `@bigduu` scope; add an npm automation token as the
  secret `NPM_TOKEN`. The package (`npm/`) downloads the matching release binary
  on install.

## License

[MIT](LICENSE) © bigduu
