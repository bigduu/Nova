# Nova

Computer Use MCP server for macOS — gives an LLM agent control of the desktop:
screenshots, mouse, keyboard, scrolling, window/app introspection, and the
clipboard, over the [Model Context Protocol](https://modelcontextprotocol.io).

Built on ScreenCaptureKit, CoreGraphics (CGEvent), and the Accessibility APIs.

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

## Coordinate grounding

A general LLM judging pixel coordinates off a downscaled screenshot is the main
source of mis-clicks. The `screenshot` tool returns a text note with the image's
exact dimensions and the coordinate contract, and offers three options to make
targeting precise. **All click/move/scroll tools work in the pixel space of the
last screenshot** — the server remembers that frame and maps clicks back to the
real screen, so the model just "clicks what it sees".

- `window: "<name>"` — capture a single window (substring of its title or app
  name) instead of the whole display. Smaller, sharper image → less context and
  far less downscaling → better precision. Later clicks map into that window.
- `grid: true` — overlay a labeled coordinate grid (rules + pixel labels every
  100px) so the model can read positions straight off the axes.
- `marks: true` — **Set-of-Mark**: draw numbered boxes over actionable UI
  elements (via the Accessibility tree) and return a list with each element's
  exact center. The model clicks a mark's listed center — the most reliable
  targeting. Needs Accessibility permission; degrades to no marks without it.

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
