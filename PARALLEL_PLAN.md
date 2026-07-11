# Platform-abstraction parallel plan

Temporary coordination doc — **delete this file before the final merge to
main** (it's for the agents doing the parallel moves, not for posterity).

## Context

`src/platform/mod.rs` now holds the capability traits (`ScreenCapture`,
`InputInjector`, `WindowManager`, `UiTree` + `ElementHandle`, `Clipboard`,
`OcrEngine`) and `src/platform/mac/` holds their macOS implementations. Only
**OCR** has actually been moved (the exemplar — see `src/platform/mac/ocr.rs`
and the `crate::platform::ocr()` accessor at the bottom of
`src/platform/mod.rs`). This doc lists the same move for the four remaining
subsystems so four agents can do it in parallel without stepping on each
other.

**Follow the OCR exemplar exactly**, for each subsystem:
1. Move the mac-specific implementation file(s) under `src/platform/mac/`,
   unchanged in substance (preserve every quirk/comment — this is a MOVE, not
   a rewrite). Add a `Mac<Capability>` struct implementing the trait, usually
   a thin wrapper around the moved free functions (see how `MacOcrEngine` just
   forwards to `recognize()` in `platform/mac/ocr.rs`).
2. Append ONE `pub mod <name>;` line to the END of `src/platform/mac/mod.rs`.
3. Append ONE `pub fn <capability>() -> &'static dyn <Trait> { .. }` accessor
   (plus its backing `static MAC_<X>: ...`) to the END of `src/platform/mod.rs`,
   mirroring the `ocr()`/`MAC_OCR` block already there.
4. Rewire the real call sites in `src/tools/*.rs` / `src/server.rs` /
   `src/main.rs` (debug CLI) to go through `crate::platform::<capability>()`
   instead of calling the old free functions directly.
5. Fix up any `tests/*.rs` that imported the old path.
6. `cargo build && cargo build --release && cargo test && cargo clippy` clean,
   `cargo fmt` on touched files only.

### Why append-only on the two shared files

`src/platform/mod.rs` and `src/platform/mac/mod.rs` are the only files every
agent touches. Each agent's edit to them is a **pure line-addition at the
end** — no reordering, no editing someone else's block. That's the cheapest
possible shape for independent branches stacked on
`refactor/platform-abstraction` to merge without textual conflicts. If you
land after another subsystem's PR merged, `git pull`/rebase first so your
append lands after theirs, not concurrently.

Do NOT: reformat either file, alphabetize the `pub mod` lines, merge two
capabilities' accessors into one function, or touch another subsystem's
block "while you're in there" — file a follow-up instead.

---

## 1. Capture → `ScreenCapture`

**Trait**: `crate::platform::ScreenCapture` (`src/platform/mod.rs`) —
`capture_display`/`capture_window(query)`/`capture_region(rect)`, all
returning `crate::capture::screenshot::RawCapture` (already OS-neutral:
`image::RgbImage` + `ViewFrame` + `Option<pid>` — do NOT redefine it, just
return it).

**Files to move** into `src/platform/mac/capture.rs` (or a `capture/` dir —
your call, it's ~2100 lines so a submodule with a few files is reasonable):
- `src/capture/broker.rs` (1196 lines) — the daemon (`run_daemon`,
  `run_worker_proxy`, `CaptureClient`, `shared_client()`, `CaptureRequest`,
  `WireWindow`, `Reply`, the flock-election/mtime/pipe-protocol plumbing).
  This is the daemon architecture memory (`bamboo-localhost-rate-limiter-bug`
  is unrelated; the relevant one is nova's own docs at the top of the file) —
  read the whole module doc before touching it, the wedge-avoidance logic is
  load-bearing.
- `src/capture/stream.rs` — `StreamCapturer` (the persistent `SCStream`
  wrapper the daemon uses internally).
- `src/capture/mod.rs`'s `init_core_graphics()` (the `CGMainDisplayID`
  bootstrap call) — this one is tiny and mac-only, move it too.

**Files that STAY put (shared/neutral, do not move)**:
- `src/capture/screenshot.rs` — `RawCapture`, `Capture`, `CaptureOptions`,
  `Mark`, `finish_capture`, `encode_jpeg_base64`, `rgba_to_rgb`,
  `STEP_TRACE`/`step()`. This is the "turn raw pixels into a finished capture"
  layer (overlays, Set-of-Mark, JPEG encode) and is already portable — it's
  the CONSUMER of `ScreenCapture`, not part of it. `finish_capture` currently
  calls `crate::tools::window::frontmost_app_pid()` and
  `crate::tools::elements::collect_actionable` — those stay as they are
  (they'll route through `WindowManager`/`UiTree` once those land).
- `src/capture/overlay.rs` — pure `image` crate drawing, no FFI. Leave as-is.

**Tool call sites to rewire**:
- `src/server.rs::acquire_capture` — currently
  `crate::capture::broker::shared_client().capture(&req)` and
  `capture_client().disconnect()` (in the timeout-backstop branch) →
  `crate::platform::screen_capture().capture_display()/.capture_window(q)/.capture_region(rect)`.
  Note the CURRENT signature takes one `CaptureRequest` enum with a `Windows`
  variant folded in (window listing goes through the SAME daemon socket as
  pixel capture, for the replayd-wedge reasons documented in
  `capture/broker.rs`). Decide whether `ScreenCapture` and `WindowManager`
  share one underlying daemon-client singleton (they should — don't spin up a
  second capture daemon connection) — e.g. both trait impls can wrap the same
  `CaptureClient` internally, they just don't need to share a Rust trait.
- `src/main.rs` — the `--selftest`/`--selftest-direct` debug paths call
  `nova::capture::broker::shared_client()`, `nova::capture::stream::StreamCapturer::new()`,
  `nova::capture::broker::any_capture_daemon_pid()` directly. These are
  low-level diagnostics for the capture daemon itself — reasonable to leave
  them calling `crate::platform::mac::capture::*` directly (bypassing the
  trait) since they're inherently mac-daemon-specific tooling, not tool-layer
  logic. Document whichever choice you make.
- `tests/e2e_screenshot.rs`, `tests/e2e_worker.rs`, `tests/e2e_capture_worker.rs`,
  `tests/e2e_ocr.rs`, `tests/e2e_safari_google.rs` import
  `nova::capture::broker::*` / `nova::capture::stream::StreamCapturer` —
  update to `nova::platform::mac::capture::*`.

**Shared files touched**: `platform/mod.rs` (append `screen_capture()`
accessor), `platform/mac/mod.rs` (append `pub mod capture;`), `lib.rs` is
unaffected (`pub mod capture;` at the crate root can stay — `screenshot.rs`
and `overlay.rs` still live there).

---

## 2. Input → `InputInjector`

**Trait**: `crate::platform::InputInjector` — `mouse_move`,
`cursor_position`, `left_click_at`/`right_click_at`/`double_click_at`,
`scroll_at`, `key_combo`, `type_text`. All take
`crate::tools::input::InputTarget` (already a neutral `Global | Pid(i32)`
enum — the trait in `platform/mod.rs` references it via that path; you may
relocate the enum's DEFINITION into `platform/mod.rs` if you prefer, but it's
not required — moving it means also updating `src/tools/batch.rs` and
`src/server.rs`, which both use `crate::tools::input::InputTarget` today).

**Files to move**: `src/tools/input.rs` (all of it — the whole file is
CoreGraphics `CGEvent`) → `src/platform/mac/input.rs`. This is the smallest
of the four remaining moves (one file, ~580 lines including its unit tests —
keep the tests, they're hermetic: key-code tables, `parse_combo`, modifier
flags, no live event posting).

Add `pub struct MacInputInjector;` implementing the trait by forwarding to the
(otherwise unchanged) free functions, exactly like `MacOcrEngine`.

**What stays / gets a thin re-export**: `crate::tools::input::InputTarget`
itself — if you leave its definition in `tools/input.rs`, that file still
needs to exist as a (much smaller) module holding just the enum, OR move the
enum too and leave `tools::input` as a `pub use crate::platform::InputTarget;`
re-export so `src/tools/batch.rs`'s `use crate::tools::input::InputTarget`
doesn't need touching. Pick one and note it in your PR description.

**Tool call sites to rewire** (`crate::tools::input::X(...)` →
`crate::platform::input().X(...)`):
- `src/server.rs`: `mouse_move`, `left_click`, `right_click`, `double_click`,
  `scroll`, `key_combo`, `type_text`, `cursor_position` tool handlers, plus
  `click_cached_mark`'s use of `cursor_position`/`left_click_at`/`mouse_move`
  (used for the coordinate-fallback + cursor-restore dance — see
  `src/server.rs` around line 277 and 335-343).
- `src/tools/batch.rs::execute_action` — every `BatchAction` arm calls
  `input::mouse_move`/`left_click_at`/etc.
- `tests/e2e_input.rs`, `tests/e2e_interaction.rs`,
  `tests/e2e_safari_google.rs` import `nova::tools::input::*` — update paths.

**Shared files touched**: `platform/mod.rs` (append `input()` accessor),
`platform/mac/mod.rs` (append `pub mod input;`).

---

## 3. Elements → `UiTree` (+ `ElementHandle`)

**This is the biggest and least mechanical of the four** — budget the most
time/review for it. `src/tools/elements/` is ~1700 lines across 10 files with
a real internal layering (see the module doc at the top of
`src/tools/elements/mod.rs`): `attrs` (raw AX FFI) → `model` (`UiElement`,
`AxHandle`, `CachedElement`) → `walk`/`hittest`/`warmth`/`geometry` (discovery
internals) → `discover` (top-level `collect_actionable`) → `actions`
(`ax_click`/`ax_set_value`/`ax_focus`) → `debug` (CLI diagnostics) →
`webclick` (AppleScript/JS click-through for browsers).

**Trait**: `crate::platform::UiTree` — `collect_actionable`, `ax_click`,
`ax_set_value`, `ax_focus`, `raise_app`, `dump_tree`, `keep_warm`/`clear_warm`.
Plus `crate::platform::ElementHandle` — the object-safe stand-in for
`AxHandle` (`click`, `is_alive`, `current_center`, `try_web_click`,
`clone_box`). `UiElement` itself (`src/tools/elements/model.rs`) is ALREADY
platform-neutral (plain `role`/`label`/`x`/`y`/`width`/`height`) — the trait
references it directly via `crate::tools::elements::UiElement`; no need to
redefine it in `platform/mod.rs`. **You have latitude to adjust
`ElementHandle`'s exact method set** if the real move surfaces something the
current draft got wrong (it was written from reading call sites, not from
doing the move) — just keep it object-safe and neutral.

**Files to move** into `src/platform/mac/elements/` (mirror the existing
submodule layout — `attrs.rs`, `model.rs`, `walk.rs`, `hittest.rs`,
`warmth.rs`, `geometry.rs`, `discover.rs`, `actions.rs`, `debug.rs`,
`webclick.rs`): basically the entire `src/tools/elements/` directory. Add
`pub struct MacUiTree;` implementing `UiTree`, and wrap `AxHandle` (from
`model.rs`) in a newtype implementing `ElementHandle` (`click`/`is_alive`/
`current_center` already exist on `AxHandle` almost verbatim; `try_web_click`
is new plumbing that combines today's `web_click_point` +
`webclick::browser_for_pid` + `webclick::js_click_at`, currently inlined in
`src/server.rs::click_cached_mark` — see below).

**The one piece of real logic to relocate, not just move**:
`src/server.rs::click_cached_mark` (around line 273-355) currently inlines the
"try web JS click, else AX click, else coordinate-click-with-cursor-restore"
decision tree. The web-JS-click branch (checking
`crate::tools::elements::web_click_point` +
`crate::tools::elements::webclick::browser_for_pid`) is exactly what
`ElementHandle::try_web_click` should encapsulate — move that branch's logic
into the mac `ElementHandle` impl so `click_cached_mark` (which stays in
`server.rs`, it's server-level orchestration, not a platform capability)
simplifies to: try `try_web_click`, else `handle.click()`, else the existing
coordinate fallback (which already calls `crate::tools::input::*` +
`crate::tools::elements::raise_app` — the `raise_app` call becomes
`crate::platform::ui_tree().raise_app(pid)`).

**Tool call sites to rewire**:
- `src/server.rs`: `render_capture` (`warmer().warm(pid)`/`.clear()` →
  `ui_tree().keep_warm(pid)`/`.clear_warm()`), `click_cached_mark` (see
  above), `ax_click`/`ax_set_value`/`ax_focus`/`dump_ax` tool handlers.
- `src/capture/screenshot.rs::build_marks` — calls
  `crate::tools::elements::collect_actionable(pid, 400, Some(clip))` →
  `crate::platform::ui_tree().collect_actionable(pid, 400, Some(clip))`. Note
  the return type changes from `Vec<(UiElement, AxHandle)>` to
  `Vec<(UiElement, Box<dyn ElementHandle>)>` — `CachedElement.handle`'s field
  type must follow (currently `AxHandle`, becomes `Box<dyn ElementHandle>`).
- `src/main.rs` debug CLI (`--dump-ax`, `--marks`, `--hit-dump`, `--ax-warm`)
  calls `tools::elements::{dump_tree, collect_actionable, hit_dump,
  ax_warm_probe}` and `tools::window::pid_for_window` directly. `hit_dump` and
  `ax_warm_probe` are diagnostics-only (not part of the MCP tool surface) —
  reasonable to leave them as `crate::platform::mac::elements::debug::*` free
  functions NOT exposed via the `UiTree` trait (document this choice, same as
  the capture agent's `--selftest` call).
- `tests/e2e_interaction.rs`, `tests/e2e_safari_google.rs` import
  `nova::tools::elements::*` — update paths.

**Shared files touched**: `platform/mod.rs` (append `ui_tree()` accessor,
and ONLY if you decide `ElementHandle` needs new methods, edit that trait
block — try to keep changes additive even there), `platform/mac/mod.rs`
(append `pub mod elements;`).

---

## 4. Window + Application → `WindowManager`

**Trait**: `crate::platform::WindowManager` — `list_windows` (returns
`Vec<crate::platform::WindowHandle>` — a NEW neutral type already defined in
`platform/mod.rs`, richer than `crate::types::WindowInfo`: carries `pid` +
`id` (CGWindowID) needed internally for AX matching), `list_applications`
(returns `Vec<crate::tools::application::ApplicationInfo>` — already neutral,
reused as-is, not redefined), `open_application`.

**Files to move**:
- `src/tools/window.rs`'s actual OS call (`shared_client().windows()` from
  `capture::broker`) — NOTE this OVERLAPS with the capture agent's work
  (window enumeration goes through the SAME daemon socket as pixel capture,
  see `capture/broker.rs`'s module doc on why). **Coordinate with whoever
  does capture** — either land after them and depend on
  `crate::platform::mac::capture`'s daemon client internally, or land first
  and have them adapt. If both land the same day, whoever merges second
  rebases. The BUSINESS logic in `tools/window.rs` (`pid_for_window`,
  `window_id_for_rect`, `frontmost_app_pid`, `is_system_ui`) stays in
  `tools/window.rs` — it's pure logic over `WindowHandle` data, not an OS
  call — just change it to call `crate::platform::window_manager().list_windows()`
  instead of `shared_client().windows()` directly, and adapt to `WindowHandle`
  field names (`w.id` instead of `w.window_id`, etc).
- `src/tools/application.rs` (`mdfind`/`open` shell-outs) → move the two
  functions' bodies into `src/platform/mac/window.rs` (or split into
  `window.rs` + `application.rs` under `platform/mac/` — your call, they're
  both small). `ApplicationInfo` the struct stays defined in
  `src/tools/application.rs` (neutral, reused by the trait) — that file can
  shrink to just the struct + maybe re-exports, or be removed with
  `ApplicationInfo` relocated to `platform/mod.rs`; note whichever you pick.

**Tool call sites to rewire**:
- `src/server.rs`: `list_windows`, `list_applications`, `open_application`
  tool handlers; `current_ax_pid`'s `tools::window::frontmost_app_pid` call
  (stays calling `tools::window::frontmost_app_pid`, which internally now
  calls the trait — no change needed at THIS call site if you keep
  `tools/window.rs`'s public functions as thin wrappers, which is the
  recommended shape here to minimize server.rs churn).
- `src/capture/screenshot.rs::finish_capture` calls
  `crate::tools::window::frontmost_app_pid()` — same as above, no change
  needed if `tools::window` keeps its function signatures and just changes
  what's inside them.
- `src/main.rs` debug CLI calls `tools::window::pid_for_window` — same,
  no signature change needed.
- `tests/e2e_interaction.rs`, `tests/e2e_safari_google.rs` import
  `nova::tools::window::list_windows`, `nova::tools::application::open_application`
  — these should keep working UNCHANGED if you keep `tools/window.rs` and
  `tools/application.rs` as the stable public surface (recommended — unlike
  the OCR exemplar, which had no other internal layer above it to preserve,
  window/application already have `tools::*` as a stable wrapper layer worth
  keeping call-compatible).

**Shared files touched**: `platform/mod.rs` (append `window_manager()`
accessor), `platform/mac/mod.rs` (append `pub mod window;`).

---

## Clipboard — deliberately NOT parallelized

`src/tools/clipboard.rs` shells out to `pbpaste`/`pbcopy` (not a
cross-platform crate) and is only ~40 lines. The `Clipboard` trait already
exists in `platform/mod.rs`; whoever gets to it first (or a 5th agent, or a
follow-up) can move it solo — it doesn't warrant its own parallel track, and
there's no meaningful coordination overhead.

## Verification checklist (every subsystem)

- `cargo build && cargo build --release` clean.
- `cargo test --workspace` — test count (passed + ignored) UNCHANGED from
  before your change; only add tests if you're genuinely adding coverage.
- `cargo clippy --workspace --all-targets` — no NEW warnings vs. `main`.
- `cargo fmt` on touched files (whole-file rustfmt of a file you didn't
  otherwise touch is noise — don't).
- `grep -rn "objc2\|core_graphics\|core_foundation\|accessibility_sys\|screencapturekit" src/tools src/server.rs src/main.rs` →
  empty (those crates should only appear under `src/platform/mac/`).
