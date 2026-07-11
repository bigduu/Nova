/// MCP server lifecycle — tool registration, transport dispatch, and handler routing.
use anyhow::{Context, Result};
use base64::Engine;
use rmcp::ServiceExt;

// ── MCP tool result helpers ─────────────────────────────────────────

/// Create a successful text result.
pub fn ok_text(msg: impl Into<String>) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(msg)])
}

/// Create an error text result (isError: true).
pub fn err_result(msg: &str) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::error(vec![rmcp::model::Content::text(msg)])
}

/// Create a successful image result with proper MCP ImageContent.
pub fn ok_image(base64_data: String, mime_type: &str) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::image(base64_data, mime_type)])
}

// ── Server state ────────────────────────────────────────────────────

/// Shared state for the Nova MCP server.
/// All tool handlers receive `&self` to this struct.
#[derive(Debug, Clone, Default)]
pub struct NovaServer {
    /// Coordinate frame of the most recent screenshot (full display or a single
    /// window). Click/move/scroll map their screenshot-space input through this,
    /// so the model always works in "the pixels of the last image it saw".
    /// `None` until the first screenshot — clicks then assume the full display.
    view: std::sync::Arc<std::sync::Mutex<Option<crate::display::view::ViewFrame>>>,
    /// Owning process id of the most recently window-captured app. When set,
    /// input is delivered straight to that process (background-style, without
    /// moving the user's cursor); when `None`, input goes to the global event
    /// stream (frontmost app). Set by `window=` captures, cleared by full-display
    /// captures, preserved across `region=` zooms.
    target_pid: std::sync::Arc<std::sync::Mutex<Option<i32>>>,
    /// Actionable elements from the most recent `marks=true` screenshot, keyed by
    /// mark number. Lets `click_mark` drive a control by the number the model saw
    /// (AX action straight on the cached handle, coordinate fallback to its
    /// center) instead of re-matching a guessed label. Replaced on each
    /// `marks=true` capture; the numbers go stale once the UI changes, so the
    /// model is told to re-shoot with `marks=true` before clicking.
    marks: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<u32, crate::tools::elements::CachedElement>>,
    >,
}

// The capture daemon's connection-drop backstop (used by `acquire_capture`
// and `list_windows` below) is an implementation detail of the macOS daemon
// client, not part of the neutral `ScreenCapture`/`WindowManager` traits (it
// has no equivalent method there) — called directly, same as the
// diagnostics-only direct calls in `main.rs`'s `--selftest`.
#[cfg(target_os = "macos")]
use crate::platform::mac::capture::broker::shared_client as capture_client;

/// Best-effort recovery hook for a capture/`list_windows` call that blew
/// through the outer timeout backstop below. On macOS this drops the shared
/// capture daemon's connection (see `platform::mac::capture::broker`) so a
/// wedged ScreenCaptureKit stream doesn't poison every subsequent call. GDI/
/// `PrintWindow` capture on Windows is synchronous with no persistent daemon
/// connection to reset — there is nothing analogous to drop, so this is a
/// no-op there (a Windows timeout here means the synchronous GDI call itself
/// hung, which no connection-reset can fix; it just surfaces as an error to
/// the model like any other capture failure).
#[cfg(target_os = "macos")]
fn reset_capture_connection() {
    capture_client().disconnect();
}
#[cfg(target_os = "windows")]
fn reset_capture_connection() {}

/// The coordinate frame of a full-display capture before any screenshot has
/// been taken yet — macOS's `CGDisplay`-derived frame, or Windows'
/// `GetSystemMetrics`-derived one.
#[cfg(target_os = "macos")]
fn default_view_frame() -> crate::display::view::ViewFrame {
    crate::platform::mac::geometry::display_view_frame()
}
#[cfg(target_os = "windows")]
fn default_view_frame() -> crate::display::view::ViewFrame {
    crate::platform::windows::geometry::display_view_frame()
}

/// One-line diagnostic logged alongside every capture, distinguishing a real
/// permission denial from a capture-stack failure. macOS has a real TCC grant
/// to report (`platform::mac::geometry::permission_diagnostics`); Windows'
/// GDI/`PrintWindow` capture needs no such grant (see
/// `platform::windows::capture`'s module doc), so there is nothing to check.
#[cfg(target_os = "macos")]
fn capture_permission_diag() -> String {
    crate::platform::mac::geometry::permission_diagnostics()
}
#[cfg(target_os = "windows")]
fn capture_permission_diag() -> String {
    "windows: no screen-recording permission concept (GDI/PrintWindow capture)".to_string()
}

/// Outer backstop on any daemon round-trip. The client's recovery ladder is
/// self-limiting (every honest daemon reply lands within QUEUE_BUDGET +
/// DAEMON_WATCHDOG, and dead daemons fail fast), but a ladder worst case —
/// three read-timeout attempts plus kills and settles — can run ~2 minutes.
/// This must sit ABOVE that so it never truncates a recovery mid-flight
/// (a truncated ladder leaves the in-flight blocking task holding the client
/// lock, making the disconnect() below a guaranteed no-op).
const CAPTURE_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(150);

impl NovaServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the coordinate frame of the screenshot just returned.
    fn set_view(&self, frame: crate::display::view::ViewFrame) {
        *self.view.lock().expect("view mutex") = Some(frame);
    }

    /// The active coordinate frame — the last screenshot's, or the full main
    /// display if no screenshot has been taken yet.
    fn current_view(&self) -> crate::display::view::ViewFrame {
        self.view
            .lock()
            .expect("view mutex")
            .unwrap_or_else(default_view_frame)
    }

    /// Convert screenshot-space coordinates (what the LLM sees) into the global
    /// logical points that mouse events are posted in, via the active view frame.
    fn to_logical(&self, x: f64, y: f64) -> (f64, f64) {
        self.current_view().to_logical(x, y)
    }

    /// The input delivery target. Default is the global event stream (foreground)
    /// — it works for every app, including browsers/Electron that ignore
    /// process-targeted events. When `background` is requested AND a window has
    /// been captured, deliver straight to that window's process instead (no
    /// cursor movement, app need not be frontmost) — reliable for native apps.
    fn current_target(&self, background: bool) -> crate::tools::input::InputTarget {
        match (
            background,
            *self.target_pid.lock().expect("target_pid mutex"),
        ) {
            (true, Some(pid)) => crate::tools::input::InputTarget::Pid(pid),
            _ => crate::tools::input::InputTarget::Global,
        }
    }

    /// Record (or clear) the process to deliver input to after a capture.
    fn set_target_pid(&self, pid: Option<i32>) {
        *self.target_pid.lock().expect("target_pid mutex") = pid;
    }

    /// The process the accessibility-action tools operate on: the last
    /// window-captured app if known, otherwise the frontmost app.
    ///
    /// Async on purpose: the frontmost-app fallback is a daemon round-trip
    /// (blocking socket I/O, worst case the full recovery ladder) — it must
    /// run on the blocking pool, and NEVER while holding the `target_pid`
    /// mutex (every input tool locks that mutex; holding it across the ladder
    /// would freeze all input tools for its duration).
    async fn current_ax_pid(&self) -> Option<i32> {
        let cached = *self.target_pid.lock().expect("target_pid mutex");
        if cached.is_some() {
            return cached;
        }
        tokio::task::spawn_blocking(crate::tools::window::frontmost_app_pid)
            .await
            .ok()
            .flatten()
    }

    /// Replace the Set-of-Mark cache with the elements of the latest `marks`
    /// capture (clearing the old numbers, which are now stale).
    fn set_marks(&self, targets: Vec<crate::tools::elements::CachedElement>) {
        let mut cache = self.marks.lock().expect("marks mutex");
        cache.clear();
        for t in targets {
            cache.insert(t.number, t);
        }
    }

    /// Look up a marked element by its number (cloned out so the AX call runs
    /// off the lock).
    fn get_mark(&self, number: u32) -> Option<crate::tools::elements::CachedElement> {
        self.marks
            .lock()
            .expect("marks mutex")
            .get(&number)
            .cloned()
    }

    /// Run the requested capture, isolating the hang-prone ScreenCaptureKit call.
    ///
    /// Phase 1 — the raw pixel capture runs behind [`crate::platform::ScreenCapture`]
    /// (on macOS: the SHARED capture daemon, one per user, all nova processes;
    /// see [`crate::platform::mac::capture::broker`] for why two same-binary
    /// ScreenCaptureKit clients wedge each other). The client call below
    /// already contains the whole recovery ladder — daemon watchdog,
    /// kill+respawn, stray-process sweep, `killall -9 replayd` — so by the time
    /// it returns an error, recovery has genuinely been attempted; the outer
    /// timeout here is only a backstop. Phase 2 — overlays + the Set-of-Mark
    /// Accessibility walk run in THIS process (the cached AX handles can't
    /// cross a process boundary), on a blocking thread with its own timeout.
    async fn acquire_capture(
        &self,
        plan: &CapturePlan,
    ) -> Result<crate::tools::screenshot::ScreenshotImage, String> {
        let region = plan.region;
        let window = plan.window.clone();
        let opts = crate::capture::screenshot::CaptureOptions {
            grid: plan.grid,
            marks: plan.marks,
        };

        // Phase 1: capture via `crate::platform::screen_capture()`. `preflight`
        // in the error distinguishes a real Screen-Recording denial (fix the
        // responsible `parent=` process) from a capture-stack failure.
        let diag = capture_permission_diag();
        // Matches the old derived-Debug rendering of the broker's
        // `CaptureRequest` (`Region { rect: (…) }` / `Window { query: "…" }`)
        // so log lines and the backstop-timeout error stay byte-identical
        // across the platform-abstraction move.
        let desc = match (region, &window) {
            (Some(rect), _) => format!("Region {{ rect: {rect:?} }}"),
            (None, Some(query)) => format!("Window {{ query: {query:?} }}"),
            (None, None) => "Display".to_string(),
        };
        tracing::info!(target: "nova::capture", "capture {desc} — {diag}");
        let task = tokio::task::spawn_blocking(move || {
            let sc = crate::platform::screen_capture();
            match (region, &window) {
                (Some(rect), _) => sc.capture_region(rect),
                (None, Some(query)) => sc.capture_window(query),
                (None, None) => sc.capture_display(),
            }
        });
        let raw = match tokio::time::timeout(CAPTURE_BACKSTOP, task).await {
            Ok(Ok(Ok(raw))) => raw,
            Ok(Ok(Err(e))) => {
                return Err(format!("screenshot capture failed: {e} [{diag}]"));
            }
            Ok(Err(join_err)) => {
                return Err(format!("capture task failed: {join_err} [{diag}]"));
            }
            Err(_) => {
                reset_capture_connection();
                return Err(format!(
                    "capture of {desc} did not return within {CAPTURE_BACKSTOP:?} — \
                     the recovery ladder itself is stuck (preflight below: \
                     preflight=false ⇒ Screen Recording not granted to the responsible \
                     `parent=` process). [{diag}]"
                ));
            }
        };

        // Phase 2: overlays + marks (Accessibility) + encode, in-process.
        let finish = tokio::task::spawn_blocking(move || {
            crate::capture::screenshot::finish_capture(raw, opts)
                .map(crate::tools::screenshot::ScreenshotImage::from)
        });
        match tokio::time::timeout(std::time::Duration::from_secs(20), finish).await {
            Ok(Ok(result)) => result,
            Ok(Err(join_err)) => Err(format!("screenshot finish task failed: {join_err}")),
            Err(_) => Err(
                "screenshot overlays/marks timed out after 20s (the accessibility walk \
                 did not complete; try again without marks)"
                    .to_string(),
            ),
        }
    }

    /// Turn a fresh capture into an MCP result: record its coordinate frame and
    /// marks, update input routing, keep the captured app's AX tree warm, and
    /// build the text note + image content.
    fn render_capture(
        &self,
        mut img: crate::tools::screenshot::ScreenshotImage,
        plan: &CapturePlan,
    ) -> rmcp::model::CallToolResult {
        // Record this image's coordinate frame so later clicks map back to the
        // right physical spot (essential for window/region captures).
        self.set_view(img.view);
        // Cache the marked elements (by number) so `click_mark` can drive them
        // directly. A `marks=true` shot replaces the set; the numbers go stale on
        // UI changes, so the model re-shoots before clicking.
        if plan.marks {
            self.set_marks(std::mem::take(&mut img.mark_targets));
        }
        // Update the input delivery target AND the AX keep-warm target. A
        // `window=` capture targets that window's process (background input) and
        // keeps its accessibility tree warm; a full-display capture clears both
        // (global input, no single app to warm); a `region=` zoom keeps whatever
        // the prior capture set (it zooms into the same surface).
        if plan.region.is_some() {
            // preserve existing target
        } else if plan.window.is_some() {
            self.set_target_pid(img.target_pid);
            // Keep the captured app's web/Electron AX tree materialized between
            // captures. Chromium/WebKit reap their semantic tree back to a
            // geometry-only skeleton once no assistive tech keeps polling, so
            // WITHOUT this the NEXT marks capture races a cold (empty) web tree
            // and silently marks only the native chrome — the "web content isn't
            // clickable" failure. The warmer only re-asserts an idempotent AX
            // enable on this one app; it never touches ScreenCaptureKit, moves
            // focus, or posts input.
            if let Some(pid) = img.target_pid {
                crate::platform::ui_tree().keep_warm(pid);
            }
        } else {
            self.set_target_pid(None);
            crate::platform::ui_tree().clear_warm();
        }
        let note = screenshot_note(&img, plan);
        rmcp::model::CallToolResult::success(vec![
            rmcp::model::Content::text(note),
            rmcp::model::Content::image(img.base64_data, img.mime_type),
        ])
    }
}

/// Click a cached marked element. Strongly prefers the Accessibility action
/// (true background: no cursor movement, no window raise, always lands on the
/// target). Only if the whole subtree/ancestry exposes no AX action does it fall
/// back to a synthesized coordinate click — and even then it first raises the
/// owning app (so the click hits the target instead of merely focusing the
/// window, the classic "first click only focuses" miss) and restores the cursor
/// afterward so the user's pointer is left where it was. Runs on a blocking
/// thread (AX + input calls block).
fn click_cached_mark(
    el: crate::tools::elements::CachedElement,
    target: crate::tools::input::InputTarget,
) -> Result<String, String> {
    let input = crate::platform::input();

    // A page refresh / navigation destroys and rebuilds the app's AX tree, so a
    // handle cached from an earlier marks shot can dangle. Detect that up front
    // and tell the model to re-shoot, rather than press a destroyed node or (via
    // a reused frame) the wrong element.
    if !el.handle.is_alive() {
        return Err(format!(
            "mark [{}] is stale — the page changed or refreshed since the marks screenshot, so its \
             numbering no longer applies. Take a fresh screenshot(marks=true) and click the new number.",
            el.number
        ));
    }

    // Web content in a scriptable browser takes a special path: AXPress on page
    // elements returns success but is a NO-OP (the page never reacts), and a
    // pid-targeted click is ignored by the browser. So drive the page's own JS
    // engine — `document.elementFromPoint(x, y).click()` — which fires the real
    // handlers while staying fully background (no cursor, app need not be
    // frontmost). Gated on BOTH the element living under an `AXWebArea` AND the
    // owning app being a scriptable browser, so native chrome (the toolbar/tabs,
    // even in Safari/Chrome) and non-browser apps keep the reliable AX path.
    match el.handle.try_web_click(el.pid, &el.label) {
        Some(Ok(desc)) => {
            return Ok(format!(
                "clicked mark [{}] {} {:?} via {desc} — background, no cursor (AXPress is a \
                 no-op on web content)",
                el.number, el.role, el.label
            ));
        }
        // JS unavailable (Automation / "allow JS from Apple Events" off) or the
        // point was empty — fall through to AX, then the coordinate path.
        Some(Err(e)) => tracing::debug!(target: "nova::click", "web JS click fell back: {e}"),
        None => {}
    }

    let ax_err = match el.handle.click() {
        Ok(action) => {
            return Ok(format!(
                "performed {action} on mark [{}] {} {:?} (via Accessibility — no cursor movement)",
                el.number, el.role, el.label
            ));
        }
        Err(e) => e,
    };

    // Coordinate fallback. Remember the cursor so we can put it back, and raise
    // the target app so the click registers on its content rather than just
    // activating the window.
    let saved = input.cursor_position().ok();
    crate::platform::ui_tree().raise_app(el.pid);
    std::thread::sleep(std::time::Duration::from_millis(120));

    let (cx, cy) = el.center;
    let click = input.left_click_at(cx, cy, target);
    if let Some((sx, sy)) = saved {
        let _ = input.mouse_move(sx, sy); // restore the user's pointer
    }
    click.map_err(|e| {
        format!(
            "mark [{}]: AX action failed ({ax_err}) and coordinate click failed: {e}",
            el.number
        )
    })?;
    Ok(format!(
        "mark [{}] {} {:?}: no AX action ({ax_err}); raised its app and coordinate-clicked the \
         center ({cx:.0}, {cy:.0}), cursor restored",
        el.number, el.role, el.label
    ))
}

/// Render the Set-of-Mark list appended to the screenshot's text note.
fn format_marks(marks: &[crate::capture::screenshot::Mark]) -> String {
    if marks.is_empty() {
        return "\nNo actionable elements detected (Accessibility permission may be missing)."
            .to_string();
    }
    let mut s = format!(
        "\n{} actionable elements — call click_mark(number=N) to activate one by its [N] \
         (background, no cursor: web content via the page's JS engine, native controls via the \
         Accessibility tree, else a click at its center):",
        marks.len()
    );
    for m in marks {
        let label = if m.label.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", m.label)
        };
        s.push_str(&format!(
            "\n  [{}] {}{} at ({:.0}, {:.0})",
            m.number, m.role, label, m.x, m.y
        ));
    }
    s
}

/// Build the text note that accompanies a capture: the subject + coordinate
/// frame (so the model never has to guess the pixel range), plus the grid legend
/// and the Set-of-Mark list when those overlays are present.
fn screenshot_note(img: &crate::tools::screenshot::ScreenshotImage, plan: &CapturePlan) -> String {
    let subject = if plan.region.is_some() {
        "a zoomed region".to_string()
    } else {
        match &plan.window {
            Some(q) => format!("window matching {q:?}"),
            None => "the main display".to_string(),
        }
    };
    let (w, h) = (img.width, img.height);
    let mut note = format!(
        "Screenshot of {subject}, {w}x{h} px. Click/move/scroll coordinates use this \
         image's pixel space: x in [0, {w}], y in [0, {h}], origin top-left. If a \
         target is too small to locate precisely, retry with marks=true to click \
         elements by number (click_mark) or zoom_region(x,y,w,h) to magnify part of it.",
    );
    if plan.grid {
        // Tell the model the grid is there and how to read it — the overlaid
        // magenta rules are easy to overlook, and the text makes the spacing
        // explicit so it reads coordinates off the nearest labeled lines instead
        // of estimating.
        let step = crate::capture::overlay::grid_step(w, h);
        note.push_str(&format!(
            "\nA magenta coordinate grid is overlaid: vertical rules every {step}px \
             labeled with their x value along the TOP and BOTTOM edges, horizontal \
             rules every {step}px labeled with their y value along the LEFT and RIGHT \
             edges. Read a target's (x, y) from the nearest labeled rules (interpolate \
             within the {step}px cell) instead of estimating from scratch."
        ));
    }
    if plan.marks {
        note.push_str(&format_marks(&img.marks));
    }
    note
}

// ── Tool implementations ────────────────────────────────────────────

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;

/// Server-level usage guidance surfaced to the model via the MCP `initialize`
/// `instructions` field. A client that injects server instructions into the
/// system prompt (bamboo does, only while nova is connected) gives the model the
/// coordinate-grounding workflow up front — which is the single biggest fix for
/// "the agent can't find the right pixel to click".
///
/// The failure this prevents (observed in real sessions): the model drives off
/// full-display screenshots, which are downscaled to ~1280px wide. On a busy or
/// Retina desktop the target app is a fraction of that frame, so list rows /
/// sidebar entries / buttons end up ~10px tall — too small to READ (it misreads
/// labels) and too small to CLICK precisely (it guesses y-coordinates and keeps
/// missing). The cure is to capture the specific window or zoom a region before
/// reading or clicking.
pub const NOVA_INSTRUCTIONS: &str = "\
Nova controls the macOS desktop: screenshots + mouse/keyboard. All click/move/\
scroll coordinates are in the pixel space of the MOST RECENT screenshot.

Targeting — how to click the right thing, in PRIORITY ORDER (do not jump to raw \
coordinates first):
1. BEST — click by mark number. A window/display `screenshot` numbers every \
actionable element BY DEFAULT (marks is on; needs Accessibility); each is listed \
as `[N] role \"label\"`. Then call `click_mark(number=N)` to activate [N]. This activates \
the control in the background with no cursor — web-page content via the page's own \
JavaScript engine (an Accessibility press is a no-op on web content), native controls \
via the Accessibility tree — falling back to a coordinate click only if neither applies. \
It is the most reliable path — \
use it whenever the element you want appears in the marks list. The numbers reset \
on every marks capture and go stale when the UI changes, so take a fresh \
`screenshot(marks=true)` right before each `click_mark`.
2. Let the app find it for you. Prefer the app's OWN search (click the search box, \
type the name, press Enter) over visually scanning a long list — far more reliable \
than estimating a row's position.
3. FALLBACK — read coordinates, only when the target is NOT in the marks list. \
Web pages are covered by marks too: real links/buttons on semantic pages, and on \
div-rendered pages (e.g. webmail) the list ROWS are numbered as well (clicking \
such a row lands via a coordinate at its center, so it still needs a fresh \
`marks=true` shot right before, since these go stale on scroll). So coordinate \
mode is mainly for canvas / game / custom-rendered surfaces that expose no marks \
at all. Then:
  - Do NOT guess off a full-display `screenshot`: it is downscaled (max ~1280px \
wide), so on a busy/Retina screen small UI is only a few pixels tall — too small \
to read or click. Capture the specific window: `screenshot(window=\"<name>\")` \
(larger, sharper, clicks map into the window automatically), and if the target is \
still small, zoom: `zoom_region(x, y, w, h)` re-captures that rectangle (in the \
last shot's pixel space) at native resolution. Click only once the target is \
clearly legible.
  - In this coordinate mode a labeled magenta grid is overlaid automatically \
(rules with their pixel x/y values along the edges): read a target's (x, y) off \
the nearest labeled rules and interpolate within the cell instead of guessing. \
(The grid is shown whenever marks is off; with marks on it is hidden since you \
click by number — pass grid=true if you want both.)
  - To READ or click text on such a marks-less surface, prefer `ocr` over \
eyeballing the grid — see \"Reading TEXT\" below.

Confirm every action — do NOT operate blind:
- After EACH input action (click, scroll, type, key press) take a screenshot to \
see the result BEFORE deciding the next action. Never fire several scrolls or \
clicks in a row without a screenshot in between — you cannot read what you \
scrolled past, and an unconfirmed click may have missed.
- When reading a long view by scrolling, scroll ONE step, screenshot, read, then \
scroll again — capturing each screen so nothing is skipped.

Keep captures focused — once you know WHICH part of the screen matters, capture \
just that part instead of the whole display:
- `screenshot(window=\"<name>\")` or `zoom_region(x, y, w, h)` returns a smaller, \
sharper image. Smaller means fewer pixels for the model to read, so each turn \
carries less context and comes back faster — and a `zoom_region` capture grabs \
only that rectangle, so it is also quicker to take than a full-display shot. \
Reserve the full-display capture for when you genuinely need the whole screen \
(orienting, finding which window to target); for repeated work inside one app or \
one panel, stay scoped to it.

Targeting a window by name (`window=\"<name>\"`):
- The name is a case-insensitive SUBSTRING of an ON-SCREEN window's title or its \
app's name, exactly as it appears on screen — match the literal on-screen text, \
do not translate or transliterate it. \
- If your guess is wrong the tool does NOT guess for you: it returns \"no \
on-screen window matching …\" and LISTS the windows that are actually on screen. \
Read that list and retry with the correct name — do not repeat the same guess. \
- When you do not already know the exact on-screen name, take a full-display \
`screenshot` first (omit window=) to read the real window/app names, then target one.

Reading TEXT — when to use `ocr`, and how to combine it with the rest:
- USE `ocr` to (a) READ a lot of text at once (a chat thread, an article, a log, \
a list/table) — it returns the lines as TEXT, far cheaper than parsing a \
screenshot image; or (b) read or click text on a surface where `marks` comes \
back EMPTY or sparse (canvas, games, image-/custom-rendered views, chat bubbles). \
Each line carries a clickable center, so left_click(x, y) a line to click text \
that is not an Accessibility element.
- Do NOT reach for `ocr` when the target IS an actionable native/web control \
(button, link, field, list row): `screenshot(marks=true)` + `click_mark` is more \
precise — it drives the control directly with no pixel guessing. And `ocr` \
returns no image, so when you need to SEE layout / icons / state, take a \
`screenshot`.
- COMBINE by role within one window: the native CHROME (sidebar, toolbar, \
buttons) is usually marked → use marks + click_mark there; the CONTENT (message \
bubbles, a rendered document) is often AX-less → use `ocr` to read or click it. \
A WeChat chat is the canonical case: marks finds only the few titlebar buttons, \
while `ocr` reads the whole conversation. Typical flow: \
`screenshot(window=\"X\", marks=true)` to act on controls, then `ocr(window=\"X\")` \
to read the content — and pass `window=\"<name>\"` (or `zoom_region` first) for \
sharper recognition of small text.

Typing:
- `type_text` accepts ANY text, including non-ASCII (e.g. 中文) and emoji. To \
enter something by name, click the field and type it directly.

Foreground vs background input:
- By DEFAULT clicks/scroll/typing go to the foreground (the real cursor moves; \
the target window is activated). This works for EVERY app, including browsers \
and Electron apps (Arc, Chrome, VS Code, Slack, WeChat). Use this unless you \
have a specific reason not to.
- For a NATIVE macOS app you do not want to disturb, pass background=true on a \
click/scroll/type to deliver it straight to the captured window's process \
without moving your cursor or raising the window. It only works after a \
`window=` capture, and browsers / Electron / custom-rendered apps IGNORE it — \
so if a background action has no visible effect, retry WITHOUT background.
- The Accessibility tree also drives controls directly, in the background, with no \
coordinates: `click_mark(number=N)` (preferred — pick the element from a \
`marks=true` shot), or by label match `ax_click`/`ax_set_value`/`ax_focus` (a \
substring of the element's role/label). The label-match tools need a semantic \
control, so they return \"no element\" on div-rendered pages (use click_mark on a \
row mark there instead) and on canvas/game surfaces with no tree.

Workflow for \"find X inside app Y\": screenshot(window=\"Y\", marks=true) → if X is \
listed, click_mark(number=N) → screenshot to confirm. If X is not in the marks, \
use Y's search box or zoom_region until X is legible, then click its coordinates \
→ screenshot to confirm.";

// Tool parameter types — all stub, to be fleshed out in implementation.

/// A resolved capture request — what to capture and how to annotate it. Both
/// the `screenshot` and `zoom_region` tools build one of these, then share the
/// capture step ([`NovaServer::acquire_capture`]) and the render step
/// ([`NovaServer::render_capture`]).
struct CapturePlan {
    /// Global-logical rect `(x, y, w, h)` for a `zoom_region` zoom; `None` for a
    /// window or full-display `screenshot`.
    region: Option<(f64, f64, f64, f64)>,
    /// Window-title/app substring for a `window=` capture; `None` otherwise.
    window: Option<String>,
    /// Overlay the coordinate grid.
    grid: bool,
    /// Draw + cache Set-of-Mark element boxes.
    marks: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// Overlay a labeled coordinate grid (magenta rules + pixel labels) for
    /// reading off click coordinates. Omitted by default it follows the AX-first
    /// rule: OFF when `marks` is on (you click by number, not coordinates), ON
    /// when `marks` is off (coordinate mode). Pass grid=true to force it on
    /// alongside marks, or grid=false to suppress it.
    #[serde(default)]
    pub grid: Option<bool>,
    /// Capture only a single on-screen window instead of the whole display —
    /// a case-insensitive substring of the window title or app name (e.g.
    /// "Safari", "Settings"). Smaller, sharper image = less context and better
    /// click precision. Subsequent clicks map to this window automatically.
    #[serde(default)]
    pub window: Option<String>,
    /// Set-of-Mark: number every actionable UI element (buttons, links, fields)
    /// and list each as `[N] role "label"`, so you can activate it with
    /// click_mark(number=N) — the most reliable targeting, no coordinate
    /// guessing. Defaults ON (AX-first). Needs Accessibility permission. Covers
    /// native controls AND web content — real links/buttons on semantic pages,
    /// and on div-rendered pages (e.g. webmail) the list rows are numbered too
    /// (their click lands via a coordinate at the row center). Only canvas/game-
    /// style surfaces with no AX come back empty. Pass marks=false for pure
    /// coordinate mode.
    #[serde(default)]
    pub marks: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ZoomRegionParams {
    /// Left edge of the rectangle, in the CURRENT image's pixel space (the last
    /// screenshot's coordinates).
    pub x: f64,
    /// Top edge of the rectangle, in the current image's pixel space.
    pub y: f64,
    /// Width of the rectangle in current-image pixels. Must be > 0.
    pub width: f64,
    /// Height of the rectangle in current-image pixels. Must be > 0.
    pub height: f64,
    /// Overlay a labeled coordinate grid. Defaults ON for a zoom (coordinate
    /// mode); pass grid=false to suppress it.
    #[serde(default)]
    pub grid: Option<bool>,
    /// Set-of-Mark numbering. Defaults OFF for a zoom — the zoom is the tool for
    /// surfaces that expose no marks, so you read coordinates off the grid. Pass
    /// marks=true to also number any actionable elements inside the region.
    #[serde(default)]
    pub marks: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MouseMoveParams {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    pub x: f64,
    pub y: f64,
    /// Deliver in the background to the captured window's process (native apps
    /// only; browsers/Electron ignore it). Default false = foreground.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
    pub x: f64,
    pub y: f64,
    pub lines: i32,
    /// Deliver in the background to the captured window's process (native apps
    /// only). Default false = foreground.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KeyParams {
    pub key: String,
    /// Deliver in the background to the captured window's process (native apps
    /// only). Default false = foreground.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeParams {
    pub text: String,
    /// Deliver in the background to the captured window's process (native apps
    /// only). Default false = foreground.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OpenAppParams {
    pub app: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitParams {
    #[serde(default = "default_duration")]
    pub duration: f64,
}

fn default_duration() -> f64 {
    1.0
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BatchParams {
    /// Ordered list of input actions to execute in a single call.
    pub actions: Vec<crate::tools::batch::BatchAction>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AxQueryParams {
    /// Case-insensitive substring matching the target element's accessibility
    /// role or label/title (e.g. "Send", "Search", "AXButton").
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AxSetValueParams {
    /// Case-insensitive substring matching the target element's role or label.
    pub query: String,
    /// The value to set (e.g. the text to place into a field).
    pub value: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickMarkParams {
    /// The mark number [N] of the element to activate, as listed by the most
    /// recent `screenshot(marks=true)`.
    pub number: u32,
    /// Deliver the coordinate-click fallback in the background to the captured
    /// window's process (native apps only). The AX action is always background;
    /// this only affects the fallback. Default false = foreground.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OcrParams {
    /// Capture only a single on-screen window (case-insensitive substring of its
    /// title or app name) instead of the whole display. Smaller, sharper image
    /// → better recognition of small text.
    #[serde(default)]
    pub window: Option<String>,
    /// BCP-47 language hints in priority order (e.g. ["zh-Hans", "en-US"]).
    /// Omitted, defaults to Simplified Chinese + English.
    #[serde(default)]
    pub languages: Option<Vec<String>>,
}

#[tool_router]
impl NovaServer {
    #[tool(
        name = "screenshot",
        description = "Capture the screen — the whole main display, or a single window with \
                       window=\"<name>\" — and return a base64 JPEG plus a text note with its pixel \
                       dimensions. ALL coordinate-taking tools (mouse_move, *_click, scroll) expect \
                       coordinates in THIS image's pixel space — origin (0,0) top-left, x right, y \
                       down — so read target positions directly off the returned image; subsequent \
                       clicks are mapped through it automatically.\n\
                       PREFER window=\"<name>\" (substring of its title or app name) over the whole \
                       display whenever you are working inside one app: a full-display shot is \
                       downscaled to ~1280px wide, so small UI (list rows, sidebar items, buttons) \
                       becomes only a few pixels — too small to read or click accurately. A window \
                       capture is larger and sharper, returns a smaller image (fewer pixels → less \
                       context, faster turn), and clicks map into the window automatically. \
                       marks is ON by default: it boxes+numbers actionable elements (needs \
                       Accessibility) and lists each as [N] — activate one with click_mark(number=N), \
                       the most reliable way to click with no coordinate guessing. A magenta \
                       coordinate grid (for reading x/y) is shown automatically when marks is off and \
                       hidden when it is on; pass grid=true to force both, or marks=false for pure \
                       coordinate mode. If a target is still too small to click, use zoom_region to \
                       magnify part of this image."
    )]
    #[tracing::instrument(skip_all, fields(window = ?p.window, grid = ?p.grid, marks = ?p.marks), level = "info")]
    async fn screenshot(
        &self,
        Parameters(p): Parameters<ScreenshotParams>,
    ) -> rmcp::model::CallToolResult {
        // AX-first default: marks ON for a window/display capture (click by
        // number). The grid follows — hidden when marks is on, shown otherwise.
        let marks = p.marks.unwrap_or(true);
        let grid = p.grid.unwrap_or(!marks);
        let plan = CapturePlan {
            region: None,
            window: p.window,
            grid,
            marks,
        };

        // Acquire the capture (blocking + timeout), then render it into a result.
        match self.acquire_capture(&plan).await {
            Ok(img) => self.render_capture(img, &plan),
            Err(e) => err_result(&e),
        }
    }

    #[tool(
        name = "zoom_region",
        description = "Zoom into a rectangle of the CURRENT image (the last screenshot's pixel \
                       space) and re-capture it at native resolution — a sharp, legible magnified \
                       view. Only that rectangle is captured (not the whole display), so it is also \
                       smaller and quicker to take than a full-display shot. Use it to read exact \
                       positions on surfaces that expose no marks (canvas/games, custom-rendered \
                       views) before clicking, or to stay scoped while working inside one area. \
                       Pass x, y, width, height in the current image's pixels (width,height > 0). \
                       The returned image becomes the new coordinate space, and clicks afterward map \
                       into the zoomed region automatically. marks defaults OFF (read coordinates off \
                       the overlaid grid); grid defaults ON. Take a screenshot first so there is an \
                       image to zoom into."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y, w = %p.width, h = %p.height, grid = ?p.grid, marks = ?p.marks), level = "info")]
    async fn zoom_region(
        &self,
        Parameters(p): Parameters<ZoomRegionParams>,
    ) -> rmcp::model::CallToolResult {
        if p.width <= 0.0 || p.height <= 0.0 {
            return err_result("width and height must be > 0");
        }
        // The rectangle is in the CURRENT image's pixel space; resolve it against
        // the active view frame into a global-logical rectangle.
        let view = self.current_view();
        let (tlx, tly) = view.to_logical(p.x, p.y);
        let (brx, bry) = view.to_logical(p.x + p.width, p.y + p.height);
        let region = (tlx, tly, brx - tlx, bry - tly);

        // A zoom is the coordinate-reading tool for surfaces with no AX tree, so
        // marks defaults OFF and the grid defaults ON.
        let marks = p.marks.unwrap_or(false);
        let grid = p.grid.unwrap_or(!marks);
        let plan = CapturePlan {
            region: Some(region),
            window: None,
            grid,
            marks,
        };

        match self.acquire_capture(&plan).await {
            Ok(img) => self.render_capture(img, &plan),
            Err(e) => err_result(&e),
        }
    }

    #[tool(
        name = "ocr",
        description = "Read on-screen TEXT via the OS text recognizer (Apple Vision). Captures the \
                       display (or window=\"<name>\") and returns the recognized text lines, each \
                       with a clickable center in the same pixel space as a screenshot — so you can \
                       both READ the text and click a line with left_click(x, y). Returns text only \
                       (no image), so it is a cheap, fast way to pull text off the screen. Best when \
                       you need to read or click TEXT on a surface where marks come back empty — \
                       canvas, games, image-rendered or custom-drawn views — or to grab a lot of \
                       text at once without parsing a screenshot. For native/web UI with an \
                       Accessibility tree, screenshot(marks=true) + click_mark is still more precise. \
                       Languages default to Simplified Chinese + English; pass languages=[...] (BCP-47) \
                       to override."
    )]
    #[tracing::instrument(skip_all, fields(window = ?p.window, languages = ?p.languages), level = "info")]
    async fn ocr(&self, Parameters(p): Parameters<OcrParams>) -> rmcp::model::CallToolResult {
        // Capture a clean image (no overlays), record its frame so the recognized
        // centers are clickable, and route input like a window/display capture.
        let plan = CapturePlan {
            region: None,
            window: p.window.clone(),
            grid: false,
            marks: false,
        };
        let img = match self.acquire_capture(&plan).await {
            Ok(img) => img,
            Err(e) => return err_result(&e),
        };
        self.set_view(img.view);
        if p.window.is_some() {
            self.set_target_pid(img.target_pid);
        } else {
            self.set_target_pid(None);
        }

        let jpeg = match base64::engine::general_purpose::STANDARD.decode(&img.base64_data) {
            Ok(bytes) => bytes,
            Err(e) => return err_result(&format!("failed to decode captured image: {e}")),
        };
        let (w, h) = (img.width, img.height);
        let languages = p
            .languages
            .clone()
            .unwrap_or_else(|| vec!["zh-Hans".to_string(), "en-US".to_string()]);

        // Vision recognition is blocking; run it off the async runtime with a
        // hard timeout so a stuck recognizer can't starve the server.
        let task = tokio::task::spawn_blocking(move || {
            let lang_refs: Vec<&str> = languages.iter().map(String::as_str).collect();
            crate::platform::ocr().recognize(&jpeg, w, h, &lang_refs)
        });
        let lines = match tokio::time::timeout(std::time::Duration::from_secs(20), task).await {
            Ok(Ok(Ok(lines))) => lines,
            Ok(Ok(Err(e))) => return err_result(&format!("OCR failed: {e}")),
            Ok(Err(join_err)) => return err_result(&format!("OCR task failed: {join_err}")),
            Err(_) => return err_result("OCR timed out after 20s"),
        };

        let subject = match &p.window {
            Some(q) => format!("window matching {q:?}"),
            None => "the main display".to_string(),
        };
        if lines.is_empty() {
            return ok_text(format!(
                "OCR of {subject} ({w}x{h} px): no text recognized."
            ));
        }
        let mut note = format!(
            "OCR of {subject} ({w}x{h} px), {n} text lines. Coordinates are in this image's pixel \
             space (same as a screenshot) — click a line by its center with left_click(x, y).\n",
            n = lines.len(),
        );
        for (i, line) in lines.iter().enumerate() {
            note.push_str(&format!(
                "  [{}] {:?} — ({:.0}, {:.0})\n",
                i + 1,
                line.text,
                line.center.0,
                line.center.1,
            ));
        }
        note.push_str("\nFull text:\n");
        for line in &lines {
            note.push_str(&line.text);
            note.push('\n');
        }
        ok_text(note)
    }

    #[tool(
        name = "mouse_move",
        description = "Move the mouse cursor to the given (x, y) coordinates (in screenshot space)."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn mouse_move(
        &self,
        Parameters(p): Parameters<MouseMoveParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().mouse_move(lx, ly) {
            Ok(()) => ok_text(format!("mouse moved to ({}, {})", p.x, p.y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    // NOTE: mouse_move always moves the real global cursor (there is no
    // background equivalent); clicks/scroll/typing below honor current_target().

    #[tool(
        name = "left_click",
        description = "Left-click at the given (x, y) coordinates (in screenshot space)."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn left_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().left_click_at(lx, ly, self.current_target(p.background)) {
            Ok(()) => ok_text(format!("left clicked at ({}, {})", p.x, p.y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "right_click",
        description = "Right-click at the given (x, y) coordinates (in screenshot space)."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn right_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().right_click_at(lx, ly, self.current_target(p.background)) {
            Ok(()) => ok_text(format!("right clicked at ({}, {})", p.x, p.y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "double_click",
        description = "Double-click at the given (x, y) coordinates (in screenshot space)."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn double_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().double_click_at(lx, ly, self.current_target(p.background)) {
            Ok(()) => ok_text(format!("double clicked at ({}, {})", p.x, p.y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "scroll",
        description = "Scroll at the given (x, y) position. Positive lines = up, negative = down."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y, lines = %p.lines), level = "info")]
    async fn scroll(&self, Parameters(p): Parameters<ScrollParams>) -> rmcp::model::CallToolResult {
        // scroll_at positions the scroll at (lx, ly): it moves the cursor there
        // for global delivery, or sets the event location for a process target.
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().scroll_at(lx, ly, p.lines, self.current_target(p.background))
        {
            Ok(()) => ok_text(format!("scrolled {} lines at ({}, {})", p.lines, p.x, p.y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "key_combo",
        description = "Simulate a key combination (e.g., \"cmd+c\", \"shift+tab\")."
    )]
    #[tracing::instrument(skip_all, fields(key = %p.key), level = "info")]
    async fn key_combo(&self, Parameters(p): Parameters<KeyParams>) -> rmcp::model::CallToolResult {
        match crate::platform::input().key_combo(&p.key, self.current_target(p.background)) {
            Ok(()) => ok_text(format!("pressed {}", p.key)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "type_text",
        description = "Type a string of text into the currently focused element."
    )]
    #[tracing::instrument(skip_all, fields(text = %p.text), level = "info")]
    async fn type_text(
        &self,
        Parameters(p): Parameters<TypeParams>,
    ) -> rmcp::model::CallToolResult {
        match crate::platform::input().type_text(&p.text, self.current_target(p.background)) {
            Ok(()) => ok_text(format!("typed \"{}\"", p.text)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "cursor_position",
        description = "Get the current mouse cursor position. Returns (x, y) in logical coordinates."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn cursor_position(&self) -> rmcp::model::CallToolResult {
        match crate::platform::input().cursor_position() {
            Ok((x, y)) => ok_text(format!("cursor at ({:.0}, {:.0})", x, y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "list_windows",
        description = "List all visible windows across all applications."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn list_windows(&self) -> rmcp::model::CallToolResult {
        // Daemon round-trip (worst case the full recovery ladder) — keep it
        // off the async runtime, with the same backstop as captures.
        let task = tokio::task::spawn_blocking(crate::tools::window::list_windows);
        match tokio::time::timeout(CAPTURE_BACKSTOP, task).await {
            Ok(Ok(Ok(windows))) => {
                ok_text(serde_json::to_string_pretty(&windows).unwrap_or_default())
            }
            Ok(Ok(Err(e))) => err_result(&e),
            Ok(Err(join_err)) => err_result(&format!("list_windows task failed: {join_err}")),
            Err(_) => {
                reset_capture_connection();
                err_result(&format!(
                    "window listing did not return within {CAPTURE_BACKSTOP:?}"
                ))
            }
        }
    }

    #[tool(
        name = "list_applications",
        description = "List all installed applications on the system."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn list_applications(&self) -> rmcp::model::CallToolResult {
        match crate::tools::application::list_applications() {
            Ok(apps) => ok_text(serde_json::to_string_pretty(&apps).unwrap_or_default()),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "open_application",
        description = "Launch or focus an application by name (e.g., \"Safari\", \"Slack\")."
    )]
    #[tracing::instrument(skip_all, fields(app = %p.app), level = "info")]
    async fn open_application(
        &self,
        Parameters(p): Parameters<OpenAppParams>,
    ) -> rmcp::model::CallToolResult {
        match crate::tools::application::open_application(&p.app) {
            Ok(()) => ok_text(format!("opened {}", p.app)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "read_clipboard",
        description = "Read the current system clipboard contents as text."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn read_clipboard(&self) -> rmcp::model::CallToolResult {
        match crate::tools::clipboard::read_clipboard() {
            Ok(text) => ok_text(text),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "write_clipboard",
        description = "Write text to the system clipboard."
    )]
    #[tracing::instrument(skip_all, fields(text = %p.text), level = "info")]
    async fn write_clipboard(
        &self,
        Parameters(p): Parameters<TypeParams>,
    ) -> rmcp::model::CallToolResult {
        match crate::tools::clipboard::write_clipboard(&p.text) {
            Ok(()) => ok_text("written to clipboard"),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "wait",
        description = "Wait for a specified number of seconds before returning."
    )]
    #[tracing::instrument(skip_all, fields(duration = %p.duration), level = "info")]
    async fn wait(&self, Parameters(p): Parameters<WaitParams>) -> rmcp::model::CallToolResult {
        tokio::time::sleep(std::time::Duration::from_secs_f64(p.duration)).await;
        ok_text(format!("waited {:.1}s", p.duration))
    }

    #[tool(
        name = "batch_actions",
        description = "Execute a sequence of input actions (mouse_move, left_click, right_click, \
                       double_click, scroll, key_combo, type_text, wait) in one call to reduce \
                       round-trips. Coordinates are in screenshot space. Take a screenshot \
                       separately afterwards to observe the result."
    )]
    #[tracing::instrument(skip_all, fields(count = %p.actions.len()), level = "info")]
    async fn batch_actions(
        &self,
        Parameters(p): Parameters<BatchParams>,
    ) -> rmcp::model::CallToolResult {
        match crate::tools::batch::execute_batch(
            p.actions,
            self.current_view(),
            self.current_target(false),
        )
        .await
        {
            Ok(results) => ok_text(results.join("\n")),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "ax_click",
        description = "Press a UI control directly through the OS accessibility tree (macOS \
                       Accessibility; Windows UI Automation is not yet implemented and returns a \
                       clear error) — no coordinates, no cursor movement, works in the background \
                       (the app need not be frontmost). `query` is a case-insensitive substring of \
                       the element's accessibility role or label/title (e.g. \"Send\", \"Search\"). \
                       Targets the last window-captured app (or the frontmost app). Only works for \
                       apps that expose an accessibility tree; if it returns \"no element \
                       matching\" (or the not-implemented error), fall back to screenshot + \
                       left_click."
    )]
    #[tracing::instrument(skip_all, fields(query = %p.query), level = "info")]
    async fn ax_click(
        &self,
        Parameters(p): Parameters<AxQueryParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no target app (take a window screenshot first)");
        };
        match crate::platform::ui_tree().ax_click(pid, &p.query) {
            Ok(msg) => ok_text(msg),
            Err(e) => err_result(&e),
        }
    }

    #[tool(
        name = "ax_set_value",
        description = "Set a control's value directly through the OS accessibility tree (macOS \
                       Accessibility; not yet implemented on Windows, where it returns a clear \
                       error) — e.g. fill a text field without focusing or typing. Background, no \
                       cursor. `query` matches the element's role/label; `value` is the text to \
                       set. Targets the last window-captured app (or frontmost). Native-app \
                       accessibility only."
    )]
    #[tracing::instrument(skip_all, fields(query = %p.query), level = "info")]
    async fn ax_set_value(
        &self,
        Parameters(p): Parameters<AxSetValueParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no target app (take a window screenshot first)");
        };
        match crate::platform::ui_tree().ax_set_value(pid, &p.query, &p.value) {
            Ok(msg) => ok_text(msg),
            Err(e) => err_result(&e),
        }
    }

    #[tool(
        name = "ax_focus",
        description = "Move keyboard focus to a control through the OS accessibility tree (macOS \
                       Accessibility; not yet implemented on Windows, where it returns a clear \
                       error). Background, no cursor. `query` matches the element's role/label. \
                       Targets the last window-captured app (or frontmost). Native-app \
                       accessibility only."
    )]
    #[tracing::instrument(skip_all, fields(query = %p.query), level = "info")]
    async fn ax_focus(
        &self,
        Parameters(p): Parameters<AxQueryParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no target app (take a window screenshot first)");
        };
        match crate::platform::ui_tree().ax_focus(pid, &p.query) {
            Ok(msg) => ok_text(msg),
            Err(e) => err_result(&e),
        }
    }

    #[tool(
        name = "click_mark",
        description = "Activate an actionable element by the mark NUMBER shown in the most recent \
                       screenshot(marks=true) — the reliable way to click without guessing \
                       coordinates. Always background, no cursor movement: web-page content in a \
                       scriptable browser (Safari, Chrome, Arc, Edge, Brave, …) is clicked through \
                       the page's OWN JavaScript engine (an Accessibility press is a silent no-op on \
                       web content), native controls through the Accessibility tree; if neither \
                       applies it falls back to a click at the element's center. Numbers go stale \
                       when the UI changes, so take a fresh screenshot(marks=true) right before \
                       calling this. If the number is unknown, re-shoot with marks=true."
    )]
    #[tracing::instrument(skip_all, fields(number = %p.number, background = %p.background), level = "info")]
    async fn click_mark(
        &self,
        Parameters(p): Parameters<ClickMarkParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(el) = self.get_mark(p.number) else {
            return err_result(&format!(
                "unknown mark [{}] — take a screenshot with marks=true first (numbers reset each \
                 marks capture)",
                p.number
            ));
        };
        let target = self.current_target(p.background);
        // AX + input calls block; run them off the async runtime.
        match tokio::task::spawn_blocking(move || click_cached_mark(el, target)).await {
            Ok(Ok(msg)) => ok_text(msg),
            Ok(Err(e)) => err_result(&e),
            Err(join_err) => err_result(&format!("click_mark task failed: {join_err}")),
        }
    }

    #[tool(
        name = "dump_ax",
        description = "DEBUG: dump the target app's Accessibility tree (roles, subroles, labels, \
                       actions, frames) as indented text — to diagnose why some elements are not \
                       marked. Targets the last window-captured app (or frontmost)."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn dump_ax(&self) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no target app (take a window screenshot first)");
        };
        match tokio::task::spawn_blocking(move || crate::platform::ui_tree().dump_tree(pid, 2500))
            .await
        {
            Ok(text) => ok_text(text),
            Err(join_err) => err_result(&format!("dump_ax task failed: {join_err}")),
        }
    }
}

// `#[tool_handler]` wires up call_tool/list_tools from the `tool_router()` above.
// We provide `get_info` ourselves so it is NOT auto-generated — letting us attach
// `instructions` (the coordinate-grounding guidance) to the `initialize` result.
#[tool_handler]
impl ServerHandler for NovaServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(NOVA_INSTRUCTIONS)
    }
}

// ── Transport runners ───────────────────────────────────────────────

/// Run the MCP server over stdio.
pub async fn run_stdio() -> Result<()> {
    tracing::info!("Starting Nova MCP server on stdio...");

    // `serve` only completes the initialize handshake and returns a running
    // service handle; the serve loop lives on that handle. Dropping it cancels
    // the service (RunningService's Drop), so we must await `waiting()` to keep
    // the process alive until the client disconnects.
    let service = NovaServer::new()
        .serve(rmcp::transport::io::stdio())
        .await
        .context("stdio server failed to initialize")?;

    let quit_reason = service.waiting().await.context("stdio server error")?;
    tracing::info!("Nova MCP server stopped: {quit_reason:?}");

    Ok(())
}

/// Run the MCP server over Streamable HTTP.
pub async fn run_http(addr: &str) -> Result<()> {
    use axum::Router;
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, tower::StreamableHttpService,
        StreamableHttpServerConfig,
    };
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    tracing::info!("Starting Nova MCP server on http://{addr}/mcp ...");

    let session_manager = Arc::new(LocalSessionManager::default());
    let app = Router::new().nest_service(
        "/mcp",
        StreamableHttpService::new(
            || Ok(NovaServer::new()),
            session_manager,
            StreamableHttpServerConfig::default(),
        ),
    );

    let addr: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid address: {addr}"))?;
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool the agent depends on must be registered. This is a hermetic
    /// check (no system APIs) that guards against a handler being renamed,
    /// dropped, or a new one being added without updating the contract.
    #[test]
    fn all_expected_tools_are_registered() {
        let router = NovaServer::tool_router();
        let expected = [
            "screenshot",
            "zoom_region",
            "ocr",
            "mouse_move",
            "left_click",
            "right_click",
            "double_click",
            "scroll",
            "key_combo",
            "type_text",
            "cursor_position",
            "list_windows",
            "list_applications",
            "open_application",
            "read_clipboard",
            "write_clipboard",
            "wait",
            "batch_actions",
            "ax_click",
            "ax_set_value",
            "ax_focus",
            "click_mark",
            "dump_ax",
        ];
        for name in expected {
            assert!(router.has_route(name), "tool not registered: {name}");
        }
        assert_eq!(
            router.list_all().len(),
            expected.len(),
            "tool count drifted from the documented contract"
        );
    }

    #[test]
    fn ok_text_is_success_with_payload() {
        let result = ok_text("hello");
        assert_eq!(result.is_error, Some(false));
        let text = result.content[0].as_text().expect("text content");
        assert_eq!(text.text, "hello");
    }

    #[test]
    fn err_result_flags_is_error() {
        let result = err_result("boom");
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0].as_text().expect("text content");
        assert_eq!(text.text, "boom");
    }

    #[test]
    fn ok_image_carries_image_content() {
        let result = ok_image("Zm9v".to_string(), "image/jpeg");
        assert_eq!(result.is_error, Some(false));
        assert!(
            result.content[0].as_image().is_some(),
            "expected image content"
        );
    }

    /// Build a minimal `ScreenshotImage` for note-formatting tests (no display).
    fn fake_image(
        width: u32,
        height: u32,
        marks: Vec<crate::capture::screenshot::Mark>,
    ) -> crate::tools::screenshot::ScreenshotImage {
        crate::tools::screenshot::ScreenshotImage {
            base64_data: "Zm9v".to_string(),
            width,
            height,
            mime_type: "image/jpeg",
            view: crate::display::view::ViewFrame {
                origin: (0.0, 0.0),
                region: (width as f64, height as f64),
                screenshot: (width as f64, height as f64),
            },
            marks,
            mark_targets: Vec::new(),
            target_pid: None,
        }
    }

    fn mark(
        number: u32,
        role: &str,
        label: &str,
        x: f64,
        y: f64,
    ) -> crate::capture::screenshot::Mark {
        crate::capture::screenshot::Mark {
            number,
            role: role.to_string(),
            label: label.to_string(),
            x,
            y,
        }
    }

    #[test]
    fn note_names_the_main_display_and_states_the_pixel_frame() {
        let img = fake_image(1280, 536, Vec::new());
        let plan = CapturePlan {
            region: None,
            window: None,
            grid: false,
            marks: false,
        };
        let note = screenshot_note(&img, &plan);
        assert!(note.contains("the main display"), "{note}");
        assert!(note.contains("1280x536 px"), "{note}");
        assert!(
            note.contains("x in [0, 1280]") && note.contains("y in [0, 536]"),
            "{note}"
        );
        // No overlays requested → no grid legend, no marks list.
        assert!(!note.contains("magenta coordinate grid"), "{note}");
        assert!(!note.contains("actionable elements"), "{note}");
    }

    #[test]
    fn note_names_the_window_subject() {
        let img = fake_image(800, 600, Vec::new());
        let plan = CapturePlan {
            region: None,
            window: Some("Safari".to_string()),
            grid: false,
            marks: false,
        };
        let note = screenshot_note(&img, &plan);
        assert!(note.contains(r#"window matching "Safari""#), "{note}");
    }

    #[test]
    fn note_names_a_zoomed_region() {
        let img = fake_image(400, 300, Vec::new());
        let plan = CapturePlan {
            region: Some((100.0, 100.0, 400.0, 300.0)),
            window: None,
            grid: false,
            marks: false,
        };
        let note = screenshot_note(&img, &plan);
        assert!(note.contains("a zoomed region"), "{note}");
    }

    #[test]
    fn note_includes_grid_legend_when_grid_on() {
        let img = fake_image(1280, 536, Vec::new());
        let plan = CapturePlan {
            region: None,
            window: None,
            grid: true,
            marks: false,
        };
        let note = screenshot_note(&img, &plan);
        assert!(note.contains("magenta coordinate grid"), "{note}");
    }

    #[test]
    fn note_lists_marks_when_marks_on() {
        let img = fake_image(1280, 536, vec![mark(1, "AXButton", "Send", 10.0, 20.0)]);
        let plan = CapturePlan {
            region: None,
            window: None,
            grid: false,
            marks: true,
        };
        let note = screenshot_note(&img, &plan);
        assert!(note.contains("[1] AXButton \"Send\""), "{note}");
        assert!(note.contains("click_mark"), "{note}");
    }

    #[test]
    fn note_reports_empty_marks_explicitly() {
        let img = fake_image(1280, 536, Vec::new());
        let plan = CapturePlan {
            region: None,
            window: None,
            grid: false,
            marks: true,
        };
        let note = screenshot_note(&img, &plan);
        assert!(note.contains("No actionable elements detected"), "{note}");
    }

    /// A zero/negative-size zoom must be rejected up front, before any capture
    /// runs — so this path is hermetic (no display / Screen Recording needed).
    #[tokio::test]
    async fn zoom_region_rejects_nonpositive_size() {
        let server = NovaServer::new();

        let zero = server
            .zoom_region(Parameters(ZoomRegionParams {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
                grid: None,
                marks: None,
            }))
            .await;
        assert_eq!(zero.is_error, Some(true));

        let negative = server
            .zoom_region(Parameters(ZoomRegionParams {
                x: 10.0,
                y: 10.0,
                width: 100.0,
                height: -5.0,
                grid: None,
                marks: None,
            }))
            .await;
        assert_eq!(negative.is_error, Some(true));
    }
}
