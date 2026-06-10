# Nova

**A Computer Use implementation in Rust.** Nova is a [Model Context
Protocol](https://modelcontextprotocol.io) server that gives an LLM agent
control of the macOS desktop: screenshots, mouse, keyboard, scrolling,
window/app introspection, and the clipboard — the "computer use" capability,
built natively in Rust rather than wrapping a Python/JS automation stack.

Built directly on Apple's ScreenCaptureKit, CoreGraphics (CGEvent), and the
Accessibility APIs — a single self-contained binary, no runtime to install.
Connect it to any MCP client (Claude Desktop, an agent runtime, your own) over
stdio or Streamable HTTP.

## Tools

| Tool | What it does |
| --- | --- |
| `screenshot` | Capture the whole display or a single `window=` — returns a JPEG + the pixel-coordinate contract. Numbers actionable elements (Set-of-Mark) by default. |
| `zoom_region` | Magnify a rectangle of the last screenshot at native resolution — reads small targets on surfaces with no Accessibility tree. |
| `click_mark` | Activate a numbered element straight through the Accessibility tree (no cursor, no pixel guessing). |
| `left_click` / `right_click` / `double_click` / `mouse_move` / `scroll` | Pointer input, in the pixel space of the last screenshot. |
| `type_text` / `key_combo` | Keyboard input (full Unicode, incl. CJK + emoji). |
| `list_windows` / `list_applications` / `open_application` | Window & app introspection. |
| `read_clipboard` / `write_clipboard` | Clipboard access. |
| `ax_click` / `ax_set_value` / `ax_focus` / `dump_ax` | Drive controls by Accessibility role/label. |
| `batch_actions` | Run a sequence of input actions in one call. |

## Requirements

- **macOS 15+** (a transitive dependency, `apple-metal`, needs the macOS 15 SDK / Xcode 16+).
- **Screen Recording** permission — for screenshots and `list_windows`. Grant it
  to the terminal/app running Nova in *System Settings → Privacy & Security →
  Screen Recording*.
- **Accessibility** permission — for posting mouse/keyboard events.

## Run

```sh
cargo run                      # stdio transport (default)
cargo run -- --http            # Streamable HTTP on 127.0.0.1:3100
cargo run -- --http --addr 0.0.0.0:8080
```

> The Swift runtime that ScreenCaptureKit links is located via an `LC_RPATH`
> baked in by `build.rs`, so no `DYLD_*` environment variable is needed for
> `cargo run`/`cargo test` or the standalone binary.

## Use it from an MCP client

Build the binary once, then point your MCP client at it.

```sh
cargo build --release      # produces target/release/nova
```

**Claude Desktop** (or any stdio MCP client) — add Nova to the client's MCP
config. For Claude Desktop that's
`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "nova": {
      "command": "/absolute/path/to/nova/target/release/nova"
    }
  }
}
```

Restart the client, then grant the **Screen Recording** and **Accessibility**
permissions when macOS prompts (or pre-grant them to the client app under
*System Settings → Privacy & Security*). The Nova tools then appear to the agent.

**HTTP clients** — run Nova as a server and connect over Streamable HTTP:

```sh
nova --http                       # 127.0.0.1:3100/mcp
nova --http --addr 0.0.0.0:8080   # reachable on the LAN
```

**First calls.** Take a `screenshot` to see the desktop and get the numbered
elements, then `click_mark(number=N)` to activate one — no coordinates needed.
Drop to `zoom_region` only when a target sits on a surface with no Accessibility
tree (canvas, games). All pointer tools use the pixel space of the most recent
screenshot, so screenshot → act → screenshot to confirm.

## Coordinate grounding

A general LLM judging pixel coordinates off a downscaled screenshot is the main
source of mis-clicks. The `screenshot` tool returns a text note with the image's
exact dimensions and the coordinate contract, and offers three options to make
targeting precise. **All click/move/scroll tools work in the pixel space of the
last screenshot** — the server remembers that frame and maps clicks back to the
real screen, so the model just "clicks what it sees".

- **Set-of-Mark (default)** — `screenshot` draws numbered boxes over actionable
  UI elements (via the Accessibility tree) and lists each one; the agent then
  calls `click_mark(number=N)` to drive it straight through Accessibility — no
  pixel guessing at all. The most reliable targeting. Needs Accessibility
  permission; degrades to plain coordinates without it.
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
| `mouse_move_roundtrips…` (`e2e_input`) | Moves the cursor, reads it back via `cursor_position`, asserts the position — restores the cursor | Accessibility |
| `click_events_post…` (`e2e_input`) | Left/right/double click on the empty desktop corner (Esc dismisses the menu) | Accessibility |
| `scroll_events_post…` (`e2e_input`) | Posts vertical scroll events | Accessibility |
| `type_text_posts…` (`e2e_input`) | **Types into the focused window** | Accessibility |
| `open_application_launches…` (`e2e_input`) | Launches/focuses System Settings | — |
| `list_windows_returns…` (`e2e_input`) | Enumerates on-screen windows | Screen Recording |
| `e2e_capture_display_returns_valid_jpeg` (`e2e_screenshot`) | Captures the display, checks the JPEG | Screen Recording |
| `e2e_capture_dims_match_target_dims_contract` (`e2e_screenshot`) | Asserts capture dims match the click-coordinate mapping | Screen Recording |

> `mouse_move_roundtrips…` is the strongest input check: it proves events
> actually reach the window server *and* that the screenshot→logical coordinate
> conversion is correct, all click paths share the same posting mechanism.

`list_applications_returns_app_bundles` (in `e2e_input`) is **not** ignored — it
only reads Spotlight and is tolerant of a Spotlight-less CI host.

### Lint & format

```sh
cargo fmt --all -- --check
cargo clippy --all-targets
```

## License

[MIT](LICENSE) © bigduu
