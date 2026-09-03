/// MCP server lifecycle — tool registration, transport dispatch, and handler routing.
use anyhow::{Context, Result};
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

const CHROME_APP_SERVICE_ONLY: &str = "Chrome semantic bridge is available only through the independent Nova.app service; configure the Nova.app connector and install the native host";

fn chrome_call_result(result: Result<serde_json::Value>) -> rmcp::model::CallToolResult {
    match result {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(json) if value.get("status").and_then(serde_json::Value::as_str) == Some("ok") => {
                ok_text(json)
            }
            // Keep the complete terminal envelope available for diagnostics,
            // but preserve MCP's failure bit so a client cannot mistake a
            // denied, stale, or ambiguous Chrome operation for success.
            Ok(json) => err_result(&json),
            Err(error) => err_result(&format!(
                "Chrome bridge result serialization failed: {error}"
            )),
        },
        Err(error) => err_result(&error.to_string()),
    }
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
    /// One generation-scoped action cache shared by `ax_read`, its `read_ui`
    /// alias, screenshot marks, `ax_activate`, and compatibility `click_mark`.
    /// A new AX read, mark-bearing capture, or activation attempt replaces
    /// the whole generation atomically.
    interaction: std::sync::Arc<std::sync::Mutex<InteractionSnapshot>>,
    next_snapshot_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Serializes generation replacement with the short validation + consume
    /// phase of an action attempt. Provider dispatch deliberately runs after
    /// releasing this gate so a wedged AX/UIA RPC cannot block future reads.
    interaction_action_gate: std::sync::Arc<std::sync::Mutex<()>>,
    /// App-owned, least-privilege semantic channel to one explicitly paired
    /// Chrome document. Ordinary stdio/HTTP sessions deliberately leave this
    /// unset; only the independent Nova.app service injects it.
    chrome_bridge: Option<nova_chrome_bridge::ChromeBridge>,
}

#[derive(Debug, Default)]
struct InteractionSnapshot {
    id: String,
    marks: std::collections::HashMap<u32, crate::tools::elements::CachedElement>,
    /// Snapshot-local semantic node id -> actionable mark. Content-only nodes
    /// deliberately have no entry and therefore cannot be activated.
    node_marks: std::collections::HashMap<String, u32>,
    nodes: std::collections::HashSet<String>,
    related_action_nodes: std::collections::HashMap<String, Vec<String>>,
}

struct AxNodeMaps {
    targets: Vec<crate::tools::elements::CachedElement>,
    node_marks: std::collections::HashMap<String, u32>,
    nodes: std::collections::HashSet<String>,
    related_action_nodes: std::collections::HashMap<String, Vec<String>>,
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
/// Headless builds never open a capture connection — nothing to reset.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
/// Headless builds have no display to derive a frame from; a degenerate
/// zero-size frame keeps the math well-defined (see `ViewFrame::to_logical`'s
/// degenerate-input handling) on the error paths that are all a headless
/// capture can take.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn default_view_frame() -> crate::display::view::ViewFrame {
    crate::display::view::ViewFrame {
        origin: (0.0, 0.0),
        region: (0.0, 0.0),
        screenshot: (0.0, 0.0),
    }
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
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture_permission_diag() -> String {
    "headless build: no desktop backend on this OS (capture always errors)".to_string()
}

/// Outer backstop on any daemon round-trip. The client's recovery ladder is
/// self-limiting (every honest daemon reply lands within QUEUE_BUDGET +
/// DAEMON_WATCHDOG, and dead daemons fail fast), but a ladder worst case —
/// three read-timeout attempts plus kills and settles — can run ~2 minutes.
/// This must sit ABOVE that so it never truncates a recovery mid-flight
/// (a truncated ladder leaves the in-flight blocking task holding the client
/// lock, making the disconnect() below a guaranteed no-op).
const CAPTURE_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(150);

/// Vision can consume substantial CPU/GPU and memory. Bound in-flight OCR so
/// concurrent MCP sessions cannot create an unbounded `spawn_blocking` backlog.
/// A timed-out task keeps its owned permit until the underlying synchronous
/// recognizer actually returns.
const OCR_MAX_CONCURRENT: usize = 2;
const OCR_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const OCR_ENCODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn ocr_gate() -> std::sync::Arc<tokio::sync::Semaphore> {
    static GATE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(OCR_MAX_CONCURRENT)))
        .clone()
}

fn ocr_run_timeout(mode: crate::platform::OcrMode) -> std::time::Duration {
    match mode {
        crate::platform::OcrMode::Fast => std::time::Duration::from_secs(10),
        crate::platform::OcrMode::Accurate => std::time::Duration::from_secs(20),
        // Auto may legitimately perform both passes.
        crate::platform::OcrMode::Auto => std::time::Duration::from_secs(28),
    }
}

impl NovaServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the app-owned Secure Chrome Bridge to this MCP session.
    pub fn with_chrome_bridge(mut self, chrome_bridge: nova_chrome_bridge::ChromeBridge) -> Self {
        self.chrome_bridge = Some(chrome_bridge);
        self
    }

    /// Run a synchronous Chrome bridge operation without blocking the async MCP
    /// transport. Sessions not hosted by Nova.app fail with one stable message
    /// rather than attempting to discover or open the privileged socket.
    async fn run_chrome_action<F>(&self, action: F) -> rmcp::model::CallToolResult
    where
        F: FnOnce(&nova_chrome_bridge::ChromeBridge) -> Result<serde_json::Value> + Send + 'static,
    {
        let Some(chrome_bridge) = self.chrome_bridge.clone() else {
            return err_result(CHROME_APP_SERVICE_ONLY);
        };
        let result = tokio::task::spawn_blocking(move || action(&chrome_bridge)).await;
        match result {
            Ok(result) => chrome_call_result(result),
            Err(error) => err_result(&format!("Chrome bridge task failed: {error}")),
        }
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

    /// The process the accessibility-action tools operate on: the last target
    /// app if still live, otherwise the focused/frontmost app. Resolution is
    /// Accessibility/UIA-only; it never asks the capture daemon.
    async fn current_ax_pid(&self) -> Option<i32> {
        let cached = *self.target_pid.lock().expect("target_pid mutex");
        let deadline = std::time::Instant::now() + READ_UI_TIMEOUT;
        tokio::task::spawn_blocking(move || {
            crate::platform::ui_tree()
                .resolve_target(None, cached, deadline)
                .ok()
                .map(|target| target.pid)
        })
        .await
        .ok()
        .flatten()
    }

    fn next_snapshot_id(&self) -> String {
        use std::sync::atomic::Ordering;
        let generation = self
            .next_snapshot_generation
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        format!("ax-{}-{generation}", std::process::id())
    }

    /// Replace the entire action cache. The caller must hold
    /// `interaction_action_gate` whenever an action could race this mutation.
    fn replace_interaction_unlocked(
        &self,
        targets: Vec<crate::tools::elements::CachedElement>,
        node_marks: std::collections::HashMap<String, u32>,
        nodes: std::collections::HashSet<String>,
        related_action_nodes: std::collections::HashMap<String, Vec<String>>,
    ) -> String {
        let id = self.next_snapshot_id();
        let marks = targets
            .into_iter()
            .map(|target| (target.number, target))
            .collect();
        *self.interaction.lock().expect("interaction mutex") = InteractionSnapshot {
            id: id.clone(),
            marks,
            node_marks,
            nodes,
            related_action_nodes,
        };
        id
    }

    /// Start a new, empty generation. Every ax_read call does this before
    /// parsing or touching a provider, so a failed/invalid/timed-out read can
    /// never leave the previous generation actionable.
    fn invalidate_interaction(&self) -> String {
        let _gate = self
            .interaction_action_gate
            .lock()
            .expect("interaction action gate");
        self.replace_interaction_unlocked(
            Vec::new(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
    }

    /// Replace the cache with screenshot marks. Each mark also receives a
    /// deterministic node id so a future structured screenshot consumer can
    /// use the same generation-safe activation protocol.
    fn set_marks(&self, targets: Vec<crate::tools::elements::CachedElement>) -> String {
        let _gate = self
            .interaction_action_gate
            .lock()
            .expect("interaction action gate");
        let node_marks = targets
            .iter()
            .map(|target| (format!("n{}", target.number), target.number))
            .collect();
        let nodes = targets
            .iter()
            .map(|target| format!("n{}", target.number))
            .collect();
        self.replace_interaction_unlocked(targets, node_marks, nodes, Default::default())
    }

    fn ax_node_maps(
        targets: Vec<(String, crate::tools::elements::CachedElement)>,
        lines: &[AxLine],
    ) -> AxNodeMaps {
        let node_marks = targets
            .iter()
            .map(|(node_id, target)| (node_id.clone(), target.number))
            .collect();
        let nodes = lines.iter().map(|line| line.node_id.clone()).collect();
        let mut related_action_nodes = std::collections::HashMap::new();
        for (index, line) in lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.mark.is_none())
        {
            let mut related = Vec::new();
            // A lower-depth predecessor is the nearest actionable ancestor in
            // a depth-first snapshot.
            if let Some(ancestor) = lines[..index]
                .iter()
                .rev()
                .find(|candidate| {
                    candidate.mark.is_some() && candidate.node.depth < line.node.depth
                })
                .map(|candidate| candidate.node_id.clone())
            {
                related.push(ancestor);
            }
            // Actionable descendants remain contiguous until the traversal
            // returns to this node's depth.
            related.extend(
                lines[index + 1..]
                    .iter()
                    .take_while(|candidate| candidate.node.depth > line.node.depth)
                    .filter(|candidate| candidate.mark.is_some())
                    .take(3)
                    .map(|candidate| candidate.node_id.clone()),
            );
            related_action_nodes.insert(line.node_id.clone(), related);
        }
        AxNodeMaps {
            targets: targets.into_iter().map(|(_, target)| target).collect(),
            node_marks,
            nodes,
            related_action_nodes,
        }
    }

    #[cfg(test)]
    fn set_ax_nodes(
        &self,
        targets: Vec<(String, crate::tools::elements::CachedElement)>,
        lines: &[AxLine],
    ) -> String {
        let _gate = self
            .interaction_action_gate
            .lock()
            .expect("interaction action gate");
        let maps = Self::ax_node_maps(targets, lines);
        self.replace_interaction_unlocked(
            maps.targets,
            maps.node_marks,
            maps.nodes,
            maps.related_action_nodes,
        )
    }

    /// Publish a completed read only if no newer read/capture superseded the
    /// empty generation reserved at request start.
    fn publish_ax_nodes(
        &self,
        generation: &str,
        targets: Vec<(String, crate::tools::elements::CachedElement)>,
        lines: &[AxLine],
    ) -> Result<String, String> {
        let _gate = self
            .interaction_action_gate
            .lock()
            .expect("interaction action gate");
        let maps = Self::ax_node_maps(targets, lines);
        let mut interaction = self.interaction.lock().expect("interaction mutex");
        if interaction.id != generation {
            return Err(format!(
                "ax_read generation {generation:?} was superseded by a newer read or capture"
            ));
        }
        interaction.marks = maps
            .targets
            .into_iter()
            .map(|target| (target.number, target))
            .collect();
        interaction.node_marks = maps.node_marks;
        interaction.nodes = maps.nodes;
        interaction.related_action_nodes = maps.related_action_nodes;
        Ok(generation.to_string())
    }

    /// Look up a marked element by its number (cloned out so the AX call runs
    /// off the lock).
    fn get_mark(&self, number: u32) -> Option<crate::tools::elements::CachedElement> {
        self.interaction
            .lock()
            .expect("interaction mutex")
            .marks
            .get(&number)
            .cloned()
    }

    fn get_ax_node(
        &self,
        snapshot_id: &str,
        node_id: &str,
    ) -> Result<crate::tools::elements::CachedElement, String> {
        let interaction = self.interaction.lock().expect("interaction mutex");
        if interaction.id != snapshot_id {
            return Err(format!(
                "stale snapshot {snapshot_id:?}; the current generation is {:?}. Run a fresh \
                 ax_read and use its snapshot_id/node_id.",
                interaction.id
            ));
        }
        let Some(mark) = interaction.node_marks.get(node_id) else {
            if interaction.nodes.contains(node_id) {
                let related = interaction
                    .related_action_nodes
                    .get(node_id)
                    .filter(|nodes| !nodes.is_empty())
                    .map(|nodes| {
                        format!(
                            " Related actionable ancestor/descendant nodes: {}.",
                            nodes.join(", ")
                        )
                    })
                    .unwrap_or_else(|| {
                        " No actionable ancestor/descendant is available in this snapshot."
                            .to_string()
                    });
                return Err(format!(
                    "node {node_id:?} is readable content but is not actionable.{related} Select \
                     an actionable node from a fresh ax_read; Nova will not invent a click."
                ));
            }
            return Err(format!(
                "node {node_id:?} is unknown in snapshot {snapshot_id:?}"
            ));
        };
        interaction
            .marks
            .get(mark)
            .cloned()
            .ok_or_else(|| format!("node {node_id:?} no longer has a live action target"))
    }

    fn activate_ax_node(
        &self,
        snapshot_id: &str,
        node_id: &str,
        target: crate::tools::input::InputTarget,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        let _gate = self
            .interaction_action_gate
            .lock()
            .expect("interaction action gate");
        let element = self.get_ax_node(snapshot_id, node_id)?;
        // Consume before crossing the process boundary. Provider calls can
        // report failure after partially applying an action, and an outer
        // timeout cannot cancel a blocking AX/UIA RPC. Releasing the short
        // coordination gate here prevents a wedged provider from wedging every
        // future read while also making this token strictly single-use.
        self.replace_interaction_unlocked(
            Vec::new(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        drop(_gate);
        match click_cached_mark(element, target, deadline) {
            Ok(message) => Ok(format!(
                "{message}; snapshot consumed — run a fresh ax_read before the next action"
            )),
            Err(error) => Err(format!(
                "{error}; snapshot was consumed before dispatch — run a fresh ax_read before \
                 retrying"
            )),
        }
    }

    fn activate_mark(
        &self,
        number: u32,
        target: crate::tools::input::InputTarget,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        let _gate = self
            .interaction_action_gate
            .lock()
            .expect("interaction action gate");
        let element = self.get_mark(number).ok_or_else(|| {
            format!(
                "unknown mark [{number}] — run ax_read (or screenshot(marks=true)) first \
                 (numbers reset each read/capture)"
            )
        })?;
        self.replace_interaction_unlocked(
            Vec::new(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        drop(_gate);
        match click_cached_mark(element, target, deadline) {
            Ok(message) => Ok(format!(
                "{message}; mark generation consumed — read/capture fresh marks before the next \
                 action"
            )),
            Err(error) => Err(format!(
                "{error}; mark generation was consumed before dispatch — read/capture fresh marks \
                 before retrying"
            )),
        }
    }

    async fn run_ax_read(&self, p: ReadUiParams) -> rmcp::model::CallToolResult {
        let invalidator = self.clone();
        let generation =
            match tokio::task::spawn_blocking(move || invalidator.invalidate_interaction()).await {
                Ok(generation) => generation,
                Err(join_error) => {
                    return err_result(&format!(
                        "capability=ax:read status=backend_failure message=\"{}\"",
                        sanitize_ax_field(&join_error.to_string(), 1_024)
                    ));
                }
            };
        let mode = match parse_ax_read_mode(p.mode.as_deref()) {
            Ok(mode) => mode,
            Err(error) => return err_result(&error),
        };
        let max_nodes = p.max.unwrap_or(DEFAULT_READ_UI_MAX).clamp(1, MAX_READ_UI);
        let max_chars = p
            .max_chars
            .unwrap_or(DEFAULT_AX_READ_CHARS)
            .clamp(4_096, MAX_AX_READ_CHARS);
        let filter = p
            .filter
            .map(|filter| filter.trim().to_lowercase())
            .filter(|filter| !filter.is_empty());
        let query = p
            .window
            .map(|query| query.trim().to_string())
            .filter(|query| !query.is_empty());
        let preferred_pid = *self.target_pid.lock().expect("target_pid mutex");
        let deadline = std::time::Instant::now() + READ_UI_TIMEOUT;

        let read = tokio::task::spawn_blocking(move || {
            let target = crate::platform::ui_tree().resolve_target(
                query.as_deref(),
                preferred_pid,
                deadline,
            )?;
            crate::platform::ui_tree().read_snapshot(
                &target,
                crate::platform::UiSnapshotOptions {
                    mode,
                    max_nodes,
                    max_chars,
                    deadline,
                },
            )
        });
        let snapshot =
            match tokio::time::timeout(READ_UI_TIMEOUT + std::time::Duration::from_secs(1), read)
                .await
            {
                Ok(Ok(Ok(snapshot))) => snapshot,
                Ok(Ok(Err(error))) => return err_result(&format_ax_error(error)),
                Ok(Err(join_error)) => {
                    return err_result(&format!(
                        "capability=ax:read status=backend_failure message=\"{}\"",
                        sanitize_ax_field(&join_error.to_string(), 1_024)
                    ));
                }
                Err(_) => {
                    return err_result(
                        "capability=ax:read status=timed_out message=\"semantic read exceeded its \
                         bounded deadline\" guidance=\"retry once; do not assume permission \
                         denial or silently switch to screenshots\"",
                    );
                }
            };

        let mut built = build_ax_entries(snapshot);
        let cached = std::mem::take(&mut built.cached);
        let publisher = self.clone();
        let publish_generation = generation.clone();
        let publish_lines = built.lines.clone();
        let snapshot_id = match tokio::task::spawn_blocking(move || {
            publisher.publish_ax_nodes(&publish_generation, cached, &publish_lines)
        })
        .await
        {
            Ok(Ok(snapshot_id)) => snapshot_id,
            Ok(Err(error)) => return err_result(&error),
            Err(join_error) => {
                return err_result(&format!(
                    "capability=ax:read status=backend_failure message=\"{}\"",
                    sanitize_ax_field(&join_error.to_string(), 1_024)
                ));
            }
        };
        self.set_target_pid(Some(built.target.pid));
        crate::platform::ui_tree().keep_warm(built.target.pid);
        ok_text(format_ax_snapshot(
            &snapshot_id,
            &built,
            mode,
            filter.as_deref(),
            max_chars,
        ))
    }

    /// Acquire unannotated pixels, isolating the hang-prone platform capture.
    ///
    /// The raw pixel capture runs behind [`crate::platform::ScreenCapture`]
    /// (on macOS: the SHARED capture daemon, one per user, all nova processes;
    /// see [`crate::platform::mac::capture::broker`] for why two same-binary
    /// ScreenCaptureKit clients wedge each other). The client call below
    /// already contains the whole recovery ladder — daemon watchdog,
    /// kill+respawn, stray-process sweep, `killall -9 replayd` — so by the time
    /// it returns an error, recovery has genuinely been attempted; the outer
    /// timeout here is only a backstop.
    async fn acquire_raw_capture(
        &self,
        region: Option<(f64, f64, f64, f64)>,
        window: Option<String>,
    ) -> Result<crate::capture::screenshot::RawCapture, String> {
        // Capture via `crate::platform::screen_capture()`. `preflight`
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
        match tokio::time::timeout(CAPTURE_BACKSTOP, task).await {
            Ok(Ok(Ok(raw))) => Ok(raw),
            Ok(Ok(Err(e))) => Err(format!("screenshot capture failed: {e} [{diag}]")),
            Ok(Err(join_err)) => Err(format!("capture task failed: {join_err} [{diag}]")),
            Err(_) => {
                reset_capture_connection();
                Err(format!(
                    "capture of {desc} did not return within {CAPTURE_BACKSTOP:?} — \
                     the recovery ladder itself is stuck (preflight below: \
                     preflight=false ⇒ Screen Recording not granted to the responsible \
                     `parent=` process). [{diag}]"
                ))
            }
        }
    }

    /// Run the requested screenshot capture. Raw acquisition is shared with
    /// OCR; only this path performs overlays/marks and base64 MCP rendering.
    async fn acquire_capture(
        &self,
        plan: &CapturePlan,
    ) -> Result<crate::tools::screenshot::ScreenshotImage, String> {
        let raw = self
            .acquire_raw_capture(plan.region, plan.window.clone())
            .await?;
        let opts = crate::capture::screenshot::CaptureOptions {
            grid: plan.grid,
            marks: plan.marks,
        };

        // Overlays + marks (Accessibility) + encode, in-process. The cached AX
        // handles cannot cross the capture-daemon process boundary.
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
    deadline: std::time::Instant,
) -> Result<String, String> {
    let input = crate::platform::input();
    el.handle.prepare_for_action(deadline)?;
    if std::time::Instant::now() >= deadline {
        return Err("semantic action deadline elapsed before validation".to_string());
    }

    // A page refresh / navigation destroys and rebuilds the app's AX tree, so a
    // handle cached from an earlier marks shot can dangle. Detect that up front
    // and tell the model to re-shoot, rather than press a destroyed node or (via
    // a reused frame) the wrong element.
    if !el.handle.is_alive() {
        return Err(format!(
            "mark [{}] is stale — the page changed or refreshed since the marks were read, so its \
             numbering no longer applies. Run a fresh ax_read and activate the new snapshot-local \
             node.",
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
    match el.handle.try_web_click(el.pid, &el.label, deadline) {
        Some(Ok(desc)) => {
            return Ok(format!(
                "route=web_dom clicked mark [{}] {} {:?} via {desc} — background, no cursor \
                 (AXPress is a \
                 no-op on web content)",
                el.number, el.role, el.label
            ));
        }
        // JS unavailable (Automation / "allow JS from Apple Events" off) or the
        // point was empty — fall through to AX, then the coordinate path.
        Some(Err(e)) => tracing::debug!(target: "nova::click", "web JS click fell back: {e}"),
        None => {}
    }

    if std::time::Instant::now() >= deadline {
        return Err("semantic action deadline elapsed before AX/UIA activation".to_string());
    }
    let ax_err = match el.handle.click() {
        Ok(action) => {
            return Ok(format!(
                "route={} performed {action} on mark [{}] {} {:?} — no cursor movement",
                semantic_action_route(),
                el.number,
                el.role,
                el.label
            ));
        }
        Err(e) => e,
    };

    // Coordinate fallback. Remember the cursor so we can put it back, and raise
    // the target app so the click registers on its content rather than just
    // activating the window.
    let saved = input.cursor_position().ok();
    crate::platform::ui_tree().raise_app(el.pid);
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err("semantic action deadline elapsed before element-center fallback".to_string());
    }
    std::thread::sleep(std::cmp::min(
        std::time::Duration::from_millis(120),
        remaining,
    ));
    if std::time::Instant::now() >= deadline {
        return Err("semantic action deadline elapsed before element-center fallback".to_string());
    }

    let Some((cx, cy)) = el.handle.current_center() else {
        return Err(format!(
            "mark [{}] no longer exposes a current frame; run a fresh ax_read before using the \
             element-center fallback",
            el.number
        ));
    };
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
        "route=element_center mark [{}] {} {:?}: no semantic action ({ax_err}); raised its app \
         and clicked the freshly verified center ({cx:.0}, {cy:.0}), cursor restored",
        el.number, el.role, el.label
    ))
}

#[cfg(target_os = "macos")]
fn semantic_action_route() -> &'static str {
    "ax"
}
#[cfg(target_os = "windows")]
fn semantic_action_route() -> &'static str {
    "uia"
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn semantic_action_route() -> &'static str {
    "unsupported"
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

// ── read_ui: AX text snapshot (no screenshot) ───────────────────────

/// Default / maximum number of elements a `read_ui` walk returns. The walk
/// budget doubles as the output cap: 200 covers a dense window, 400 matches the
/// `screenshot(marks=true)` walk budget and bounds a pathological tree.
const DEFAULT_READ_UI_MAX: usize = 200;
const MAX_READ_UI: usize = 400;
const DEFAULT_AX_READ_CHARS: usize = 30_000;
const MAX_AX_READ_CHARS: usize = 100_000;
/// Bound on the `read_ui` AX walk, mirroring the screenshot marks-walk timeout —
/// a cold web tree or a wedged app must not hang the tool.
const READ_UI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Generation validation, provider-side activation, and any fallback share one
/// deadline. The generation is consumed before provider dispatch; platform
/// handles configure AX/UIA RPC timeouts from this value.
const AX_ACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// One entry in a `read_ui` text listing: the click-by-number contract WITHOUT
/// an image. Kept separate from [`crate::capture::screenshot::Mark`] (which
/// carries screenshot-pixel coordinates a text read has no use for) so the
/// renderer stays trivially unit-testable with no display/AX handle.
#[derive(Debug, Clone)]
pub struct UiLine {
    pub number: u32,
    pub role: String,
    pub label: String,
    pub value: String,
}

/// Split the AX walk output into the click cache (keyed by mark number) AND the
/// text lines, in ONE pass so the `[N]` printed in the listing and the
/// `set_marks` key always agree. Mirrors `build_marks` (screenshot.rs) minus the
/// pixel projection + overlay drawing: numbers are 1-based in walk order,
/// `center` is the element's global-logical midpoint (the coordinate-click
/// fallback), and `value` is carried through for text controls. Platform-neutral
/// (both `UiElement`/`CachedElement` re-exports share these fields), so it
/// compiles unchanged for the Windows target.
pub fn build_ui_entries(
    elements: Vec<(
        crate::tools::elements::UiElement,
        Box<dyn crate::platform::ElementHandle>,
    )>,
    pid: i32,
) -> (Vec<crate::tools::elements::CachedElement>, Vec<UiLine>) {
    let mut cached = Vec::with_capacity(elements.len());
    let mut lines = Vec::with_capacity(elements.len());
    for (el, handle) in elements {
        let number = cached.len() as u32 + 1;
        let center = el.center();
        lines.push(UiLine {
            number,
            role: el.role.clone(),
            label: el.label.clone(),
            value: el.value.clone(),
        });
        cached.push(crate::tools::elements::CachedElement {
            number,
            handle,
            center,
            role: el.role,
            label: el.label,
            pid,
        });
    }
    (cached, lines)
}

/// Trim a control's value for the listing so a long text field can't blow up the
/// note. Char-based (not byte) so multi-byte text is never split mid-codepoint;
/// newlines/carriage-returns/tabs collapse to spaces to keep one element on one line.
fn truncate_value(v: &str) -> String {
    const MAX: usize = 80;
    let flat = v.replace(['\n', '\r', '\t'], " ");
    if flat.chars().count() <= MAX {
        flat
    } else {
        let head: String = flat.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// Render the `read_ui` text listing: one `[N] role "label"` line per element
/// (plus `= "value"` for text controls). Shares the `[N] role "label"` token
/// shape with [`format_marks`] so `click_mark`'s numbering contract is identical
/// whether the marks came from `read_ui` or `screenshot(marks=true)`. With a
/// `filter`, only matching lines are shown but their ORIGINAL numbers are kept
/// (so `click_mark(N)` still resolves against the full cache) and the header
/// reports shown/total.
pub fn format_ui_listing(lines: &[UiLine], subject: &str, filter: Option<&str>) -> String {
    if lines.is_empty() {
        return format!(
            "read_ui of {subject}: no actionable elements found (the app may expose no \
             accessibility tree — try screenshot(marks=true), or `ocr` to read plain text)."
        );
    }
    let shown: Vec<&UiLine> = match filter {
        Some(f) => lines
            .iter()
            .filter(|l| {
                l.role.to_lowercase().contains(f)
                    || l.label.to_lowercase().contains(f)
                    || l.value.to_lowercase().contains(f)
            })
            .collect(),
        None => lines.iter().collect(),
    };
    if shown.is_empty() {
        return format!(
            "read_ui of {subject}: none of {} actionable elements match filter {:?} \
             (re-run without filter to see them all).",
            lines.len(),
            filter.unwrap_or("")
        );
    }
    let header = match filter {
        Some(f) => format!(
            "read_ui of {subject}: {} of {} actionable elements match {f:?}",
            shown.len(),
            lines.len()
        ),
        None => format!("read_ui of {subject}: {} actionable elements", lines.len()),
    };
    let mut s = format!(
        "{header}. Activate one with click_mark(number=N) — no screenshot needed \
         (background, no cursor). Take a screenshot only to VERIFY a visual result or to read a \
         marks-less surface; re-run read_ui after the UI changes (numbers reset each read):"
    );
    for l in shown {
        let label = if l.label.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", l.label)
        };
        let value = if l.value.is_empty() {
            String::new()
        } else {
            format!(" = \"{}\"", truncate_value(&l.value))
        };
        s.push_str(&format!("\n  [{}] {}{}{}", l.number, l.role, label, value));
    }
    s
}

#[derive(Debug, Clone)]
struct AxLine {
    node_id: String,
    mark: Option<u32>,
    node: crate::platform::UiNode,
}

struct BuiltAxEntries {
    target: crate::platform::UiTarget,
    coverage: crate::platform::UiReadCoverage,
    truncated: bool,
    partial_reason: Option<crate::platform::UiPartialReason>,
    cached: Vec<(String, crate::tools::elements::CachedElement)>,
    lines: Vec<AxLine>,
}

fn build_ax_entries(snapshot: crate::platform::UiSnapshot) -> BuiltAxEntries {
    let mut cached = Vec::new();
    let mut lines = Vec::with_capacity(snapshot.nodes.len());
    for (index, mut collected) in snapshot.nodes.into_iter().enumerate() {
        let node_id = format!("n{}", index + 1);
        let mark = match collected.handle.take() {
            Some(handle) if collected.node.actionable => {
                let number = cached.len() as u32 + 1;
                let label = if !collected.node.name.is_empty() {
                    collected.node.name.clone()
                } else if !collected.node.description.is_empty() {
                    collected.node.description.clone()
                } else {
                    collected.node.value.as_filter_text().to_string()
                };
                cached.push((
                    node_id.clone(),
                    crate::tools::elements::CachedElement {
                        number,
                        handle,
                        // Legacy field retained for screenshot-mark ABI. The
                        // action path always asks the live handle for a fresh
                        // center and can invoke a semantic control with no
                        // bounds at all.
                        center: collected
                            .node
                            .bounds
                            .map(crate::platform::UiBounds::center)
                            .unwrap_or((0.0, 0.0)),
                        role: collected.node.role.clone(),
                        label,
                        pid: snapshot.target.pid,
                    },
                ));
                Some(number)
            }
            _ => None,
        };
        lines.push(AxLine {
            node_id,
            mark,
            node: collected.node,
        });
    }
    BuiltAxEntries {
        target: snapshot.target,
        coverage: snapshot.coverage,
        truncated: snapshot.truncated,
        partial_reason: snapshot.partial_reason,
        cached,
        lines,
    }
}

/// Keep the model-facing snapshot one-line-per-node and prevent any control
/// character from forging headers or log-like output. Limits count Unicode
/// scalar values, never bytes.
fn sanitize_ax_field(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for character in value.chars().take(max_chars) {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            character if character.is_control() => out.push(' '),
            character => out.push(character),
        }
    }
    if value.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn ax_line_matches(line: &AxLine, filter: &str) -> bool {
    line.node.role.to_lowercase().contains(filter)
        || line.node.name.to_lowercase().contains(filter)
        || line.node.description.to_lowercase().contains(filter)
        || line
            .node
            .value
            .as_filter_text()
            .to_lowercase()
            .contains(filter)
        || line
            .node
            .actions
            .iter()
            .any(|action| action.to_lowercase().contains(filter))
}

fn parse_ax_read_mode(mode: Option<&str>) -> Result<crate::platform::UiReadMode, String> {
    match mode.map(str::trim).filter(|mode| !mode.is_empty()) {
        None | Some("all") => Ok(crate::platform::UiReadMode::All),
        Some("interactive") => Ok(crate::platform::UiReadMode::Interactive),
        Some("content") => Ok(crate::platform::UiReadMode::Content),
        Some(other) => Err(format!(
            "invalid ax_read mode {other:?}; expected \"interactive\", \"content\", or \"all\""
        )),
    }
}

fn format_ax_node(line: &AxLine) -> String {
    let mark = line
        .mark
        .map(|mark| format!(" mark={mark}"))
        .unwrap_or_default();
    let mut rendered = format!(
        "[{}{}] depth={} role=\"{}\"",
        line.node_id,
        mark,
        line.node.depth,
        sanitize_ax_field(&line.node.role, 128)
    );
    if !line.node.name.is_empty() {
        rendered.push_str(&format!(
            " name=\"{}\"",
            sanitize_ax_field(&line.node.name, 4_096)
        ));
    }
    if !line.node.description.is_empty() {
        rendered.push_str(&format!(
            " description=\"{}\"",
            sanitize_ax_field(&line.node.description, 4_096)
        ));
    }
    match &line.node.value {
        crate::platform::UiNodeValue::Absent => {}
        crate::platform::UiNodeValue::Text(value) => {
            rendered.push_str(&format!(" value=\"{}\"", sanitize_ax_field(value, 4_096)))
        }
        crate::platform::UiNodeValue::Redacted => rendered.push_str(" value=\"[REDACTED]\""),
    }
    rendered.push_str(&format!(" actionable={}", line.node.actionable));
    if !line.node.actions.is_empty() {
        rendered.push_str(" actions=[");
        rendered.push_str(
            &line
                .node
                .actions
                .iter()
                .map(|action| format!("\"{}\"", sanitize_ax_field(action, 128)))
                .collect::<Vec<_>>()
                .join(","),
        );
        rendered.push(']');
    }
    let states = [
        ("enabled", line.node.states.enabled),
        ("focused", line.node.states.focused),
        ("selected", line.node.states.selected),
        ("checked", line.node.states.checked),
        ("expanded", line.node.states.expanded),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| format!("{name}={value}")))
    .collect::<Vec<_>>();
    if !states.is_empty() {
        rendered.push_str(&format!(" states={{{}}}", states.join(",")));
    }
    if let Some(bounds) = line.node.bounds {
        rendered.push_str(&format!(
            " bounds=({:.1},{:.1},{:.1},{:.1})",
            bounds.x, bounds.y, bounds.width, bounds.height
        ));
    }
    rendered
}

fn format_ax_snapshot(
    snapshot_id: &str,
    built: &BuiltAxEntries,
    mode: crate::platform::UiReadMode,
    filter: Option<&str>,
    max_chars: usize,
) -> String {
    let shown: Vec<_> = built
        .lines
        .iter()
        .filter(|line| filter.is_none_or(|filter| ax_line_matches(line, filter)))
        .collect();
    let mut body = String::new();
    let mut rendered_count = 0usize;
    // Reserve enough room for the bounded metadata header and fallback
    // instruction, so the final hard guard cannot silently invalidate the
    // header's explicit `truncated` flag.
    let body_budget = max_chars.saturating_sub(1_600);
    for line in &shown {
        let rendered = format_ax_node(line);
        let added = rendered.chars().count() + 1;
        if body.chars().count().saturating_add(added) > body_budget {
            break;
        }
        body.push('\n');
        body.push_str(&rendered);
        rendered_count += 1;
    }
    let render_truncated = rendered_count < shown.len();
    let truncated = built.truncated || render_truncated;
    let reason = if render_truncated {
        Some("character_limit")
    } else {
        built
            .partial_reason
            .map(crate::platform::UiPartialReason::as_str)
    };
    let filter_note = filter
        .map(|_| {
            format!(
                " filter_applied=true shown={}/{}",
                rendered_count,
                built.lines.len()
            )
        })
        .unwrap_or_else(|| format!(" shown={rendered_count}/{}", built.lines.len()));
    let mut output = format!(
        "capability=ax:read snapshot_id=\"{}\" mode={} coverage={} truncated={}{} \
         target={{pid={},app=\"{}\",window=\"{}\"}}{}",
        sanitize_ax_field(snapshot_id, 128),
        mode.as_str(),
        built.coverage.as_str(),
        truncated,
        reason
            .map(|reason| format!(" partial_reason={reason}"))
            .unwrap_or_default(),
        built.target.pid,
        sanitize_ax_field(&built.target.app_name, 256),
        sanitize_ax_field(&built.target.window_title, 512),
        filter_note,
    );
    if built.coverage != crate::platform::UiReadCoverage::Complete {
        output.push_str(
            " fallback=use_focused_ocr_for_missing_rendered_text_then_screenshot_or_zoom_for_\
             visual_only_state",
        );
    }
    output.push_str(
        ". Action: call ax_activate(snapshot_id, node_id) for an actionable node. Every activation \
         attempt consumes the generation before provider dispatch; rerun ax_read after any result.",
    );
    output.push_str(&body);
    if output.chars().count() > max_chars {
        output = output.chars().take(max_chars.saturating_sub(1)).collect();
        output.push('…');
    }
    output
}

fn format_ax_error(error: crate::platform::UiReadError) -> String {
    let guidance = match error.kind {
        crate::platform::UiReadErrorKind::PermissionDenied => {
            "grant Accessibility permission and retry; do not fall back to screenshot/OCR"
        }
        crate::platform::UiReadErrorKind::TargetNotFound => {
            "list the actual apps/windows or pass a different window query"
        }
        crate::platform::UiReadErrorKind::NoSemanticTree => {
            "use focused OCR for rendered text, then screenshot/zoom for visual-only state"
        }
        crate::platform::UiReadErrorKind::TimedOut => {
            "retry once; if the provider remains unavailable, inspect or restart the target app"
        }
        crate::platform::UiReadErrorKind::UnsupportedPlatform => {
            "use another supported Nova desktop backend"
        }
        crate::platform::UiReadErrorKind::BackendFailure => {
            "retry after checking the target app and Accessibility provider"
        }
    };
    format!(
        "capability=ax:read status={} message=\"{}\" guidance=\"{}\"",
        error.kind.as_str(),
        sanitize_ax_field(&error.message, 1_024),
        guidance
    )
}

// ── Tool implementations ────────────────────────────────────────────

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;

/// Server-level usage guidance surfaced to the model via the MCP `initialize`
/// `instructions` field. A client that injects server instructions into the
/// system prompt (bamboo does, only while nova is connected) gives the model the
/// AX-first targeting workflow up front — which is the single biggest fix for
/// "the agent can't find the right thing to click".
///
/// The workflow it anchors: read the UI as TEXT with `read_ui` (the Accessibility
/// tree, no image), act by number with `click_mark`, and take a screenshot only
/// to VERIFY a visual result or to drive a surface with no tree. This keeps most
/// steps off the image path entirely.
///
/// The failure it prevents (observed in real sessions): the model drives off
/// full-display screenshots, which are downscaled to ~1280px wide. On a busy or
/// Retina desktop the target app is a fraction of that frame, so list rows /
/// sidebar entries / buttons end up ~10px tall — too small to READ (it misreads
/// labels) and too small to CLICK precisely (it guesses y-coordinates and keeps
/// missing). `read_ui` sidesteps this (it reads labels exactly, clicks by number);
/// when a screenshot IS needed, capturing the specific window or zooming a region
/// is the cure.
pub const NOVA_INSTRUCTIONS: &str = "\
Nova controls the macOS and Windows desktop. `ax_read` is the canonical `ax:read` \
capability and the FIRST operation for labels, controls, fields, structured text, and \
semantic state. It reads Accessibility/UIA directly without a screenshot; `read_ui` is \
only a compatibility alias.

For routine Chrome page automation and debugging, prefer the official Chrome DevTools MCP \
server when it is connected. Use Nova for browser chrome, permission dialogs, other desktop \
apps, and visual fallback. Nova's separately installed Secure Chrome Bridge remains the \
least-privilege option when a user explicitly pairs one page and broad profile access is \
not acceptable.

When using the Secure Chrome Bridge, first call `chrome_status`. When the user has \
explicitly paired the active page, prefer `chrome_read` and the exact `chrome_activate`, \
`chrome_focus`, `chrome_set_value`, or `chrome_scroll` tools over browser AX and pixels. Pairing is bound \
to one tab/document/page nonce/epoch and is revoked by navigation or disconnect. Run a \
fresh `chrome_read` immediately before every mutation; Chrome semantic tools never accept \
or fall back to screen coordinates. If unpaired, call `chrome_pair` and ask the user to \
confirm the origin in the Nova extension popup within 30 seconds.

READ in this order:
1. Call `ax_read(window?, mode=\"all\")`. Respect its coverage/status. \
`permission_denied` means grant Accessibility and retry — do NOT hide it with OCR or a \
screenshot.
2. If AX coverage is absent/partial and the missing information is rendered TEXT, call \
focused-window `ocr`; its returned center is the grounded click point for that text.
3. Use focused `screenshot(window=...)` / `zoom_region` only when pixels are necessary: \
layout, icon, color, image, canvas, or visual verification. Raw coordinates are last.

ACT in this order:
1. Immediately before acting, run a fresh `ax_read`, then call \
`ax_activate(snapshot_id, node_id)` on the exact actionable node. It fails closed on a \
stale generation and reports route=ax|uia|web_dom|element_center. Every activation attempt \
consumes the generation before provider dispatch, so rerun ax_read after any result.
2. Let ax_activate use its freshly verified element-center fallback when semantic \
activation is unsupported. For AX-less rendered text, use the center returned by OCR.
3. Only then click coordinates from a focused screenshot/zoom. Never guess from a \
downscaled full-display image. `click_mark(number=N)` remains a compatibility action; \
prefer generation-safe `ax_activate`. Legacy substring `ax_click`/`ax_focus`/\
`ax_set_value` reject ambiguous matches instead of choosing the first.

VERIFY after every action before continuing. Prefer `ax_read` for semantic outcomes \
(text/state/new controls). Use a screenshot only when the expected outcome is genuinely \
visual. When scrolling through content, scroll one step, read/observe it, then continue.

All click/move/scroll coordinates use the pixel space of the MOST RECENT screenshot. \
Keep image captures focused on one window or region. `type_text` accepts Unicode. \
Foreground input is the universal default; background input is best-effort for native \
apps and may be ignored by browsers/custom-rendered apps.";

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
#[serde(rename_all = "snake_case")]
pub enum CoordinateActionSource {
    /// Center returned by the immediately preceding focused OCR result.
    OcrCenter,
    /// Coordinate read from the immediately preceding focused screenshot/zoom.
    VisualCoordinate,
}

impl CoordinateActionSource {
    fn as_route(&self) -> &'static str {
        match self {
            Self::OcrCenter => "ocr_center",
            Self::VisualCoordinate => "visual_coordinate",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    pub x: f64,
    pub y: f64,
    /// Deliver in the background to the captured window's process (native apps
    /// only; browsers/Electron ignore it). Default false = foreground.
    #[serde(default)]
    pub background: bool,
    /// Grounding source for route reporting. Use `ocr_center` only for a center
    /// returned by the immediately preceding OCR result. Omit/default
    /// `visual_coordinate` for a focused screenshot/zoom coordinate.
    #[serde(default)]
    pub source: Option<CoordinateActionSource>,
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
    /// recent `read_ui` or `screenshot(marks=true)`.
    pub number: u32,
    /// Deliver the coordinate-click fallback in the background to the captured
    /// window's process (native apps only). The AX action is always background;
    /// this only affects the fallback. Default false = foreground.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AxActivateParams {
    /// Ephemeral snapshot id returned by the immediately preceding ax_read.
    pub snapshot_id: String,
    /// Snapshot-local node id (for example "n7"). The node must be actionable.
    pub node_id: String,
    /// Applies only to the verified element-center fallback. Semantic AX/UIA/DOM
    /// activation is already background and cursor-free.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OcrModeParam {
    /// Fast first; retry accurately only for empty/low-confidence Vision output.
    Auto,
    /// Lowest latency, potentially lower recognition quality.
    Fast,
    /// Highest quality, potentially slower.
    Accurate,
}

impl From<OcrModeParam> for crate::platform::OcrMode {
    fn from(value: OcrModeParam) -> Self {
        match value {
            OcrModeParam::Auto => Self::Auto,
            OcrModeParam::Fast => Self::Fast,
            OcrModeParam::Accurate => Self::Accurate,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OcrRoiParams {
    /// Left edge in the CURRENT image's pixel space.
    pub x: f64,
    /// Top edge in the CURRENT image's pixel space.
    pub y: f64,
    /// ROI width in current-image pixels; must be finite and > 0.
    pub width: f64,
    /// ROI height in current-image pixels; must be finite and > 0.
    pub height: f64,
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
    /// Recognition latency/quality policy. `auto` (default) starts with the
    /// fast Apple Vision recognizer and falls back to accurate only for empty
    /// or low-confidence output. On engines without a native mode knob the
    /// setting preserves that engine's normal behavior.
    #[serde(default)]
    pub mode: Option<OcrModeParam>,
    /// Optional strict region of interest in the CURRENT image's pixel space.
    /// Nova maps it back to global logical coordinates and re-captures through
    /// the platform's native region path before JPEG encoding/OCR. It is never
    /// cropped from an already-downscaled JPEG. Every value must be finite, the
    /// size positive, and the entire rectangle in bounds. Mutually exclusive
    /// with `window`.
    #[serde(default)]
    pub roi: Option<OcrRoiParams>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadUiParams {
    /// Target a single on-screen window: a case-insensitive substring of its
    /// title or app name (e.g. "Safari", "Settings"). Omit to read the current
    /// target app — the last window-captured or read_ui'd app, otherwise the
    /// frontmost. Web-page content is included either way (as long as the app has
    /// a window); a window= just narrows and sharpens the listing to that window.
    #[serde(default)]
    pub window: Option<String>,
    /// Show only elements whose role, label, or value contains this
    /// case-insensitive substring (e.g. "button", "search", "submit"). Omit to
    /// list everything. Filtering only narrows the DISPLAY — the hidden elements
    /// keep their numbers and stay clickable by click_mark(N).
    #[serde(default)]
    pub filter: Option<String>,
    /// Cap on the number of elements returned (default 200, max 400).
    #[serde(default)]
    pub max: Option<usize>,
    /// `interactive` returns actionable controls, `content` returns readable
    /// labels/text, and `all` combines both in deterministic tree order.
    /// Defaults to `all`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Unicode-character output budget (default 30000, max 100000). If hit,
    /// `truncated=true` and `partial_reason=character_limit` are returned.
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChromeReadParams {
    /// Maximum semantic nodes returned by the extension (default 500, hard
    /// capped by the content script at 1000).
    #[serde(default)]
    pub max_nodes: Option<u64>,
    /// Maximum Unicode-character budget (default 100000, hard capped at
    /// 500000). The result reports truncation rather than silently omitting it.
    #[serde(default)]
    pub max_chars: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChromeNodeParams {
    /// Ephemeral snapshot returned by the immediately preceding chrome_read.
    pub snapshot_id: String,
    /// Exact snapshot-local semantic node id.
    pub node_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChromeSetValueParams {
    /// Ephemeral snapshot returned by the immediately preceding chrome_read.
    pub snapshot_id: String,
    /// Exact snapshot-local semantic node id for a non-sensitive text control.
    pub node_id: String,
    /// New field value. Nova never logs or echoes this plaintext; the result
    /// contains only its UTF-8 length and SHA-256 digest.
    pub value: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChromeScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl ChromeScrollDirection {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChromeScrollAmount {
    Line,
    HalfPage,
    Page,
}

impl ChromeScrollAmount {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::HalfPage => "half_page",
            Self::Page => "page",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChromeScrollParams {
    /// Ephemeral snapshot returned by the immediately preceding chrome_read.
    pub snapshot_id: String,
    /// Exact snapshot-local semantic node id, or `root` when exposed by the
    /// document snapshot.
    pub node_id: String,
    pub direction: ChromeScrollDirection,
    pub amount: ChromeScrollAmount,
}

#[tool_router]
impl NovaServer {
    #[tool(
        name = "screenshot",
        description = "Visual fallback after ax_read and, for missing rendered text, OCR. Capture \
                       the whole main display or a single window with \
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
                       Accessibility) and lists each as [N] for compatibility with click_mark. \
                       Prefer a fresh ax_read + ax_activate for semantic controls. A magenta \
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
        description = "Read on-screen TEXT via the platform OCR engine (Apple Vision on macOS, \
                       Windows.Media.Ocr on Windows). Captures the \
                       display (or window=\"<name>\") and returns the recognized text lines, each \
                       with a clickable center in the same pixel space as a screenshot — so you can \
                       both READ the text and click a line with left_click(x, y). Returns text only \
                       (no image), so it is a cheap, fast way to pull text off the screen. Best when \
                       use it after ax_read reports absent/partial coverage and you need rendered \
                       TEXT from a surface where semantic nodes are empty or sparse — \
                       canvas, games, image-rendered or custom-drawn views — or to grab a lot of \
                       text at once without parsing a screenshot. For native/web UI with an \
                       Accessibility/UIA tree, ax_read + ax_activate is still more precise. \
                       mode=auto (default) tries fast recognition first and only falls back to \
                       accurate for empty/low-confidence output; mode=fast minimizes latency and \
                       mode=accurate forces maximum quality. roi={x,y,width,height} scopes OCR in \
                       the CURRENT image's pixels: the rectangle is strictly validated, mapped to \
                       the desktop, and re-captured through the native region path (never cropped \
                       from a downscaled JPEG). roi and window are mutually exclusive. \
                       Languages default to Simplified Chinese + English; pass languages=[...] (BCP-47) \
                       to override."
    )]
    #[tracing::instrument(skip_all, fields(window = ?p.window, roi = ?p.roi, mode = ?p.mode, languages = ?p.languages), level = "info")]
    async fn ocr(&self, Parameters(p): Parameters<OcrParams>) -> rmcp::model::CallToolResult {
        if p.window.is_some() && p.roi.is_some() {
            return err_result("ocr window and roi are mutually exclusive; pass only one");
        }

        // A ROI is expressed against the current image, exactly like
        // `zoom_region`, but is stricter: never clamp it. Native region capture
        // retains text detail that post-cropping the already-downscaled display
        // JPEG would irreversibly lose.
        let native_roi = match &p.roi {
            Some(roi) => match self
                .current_view()
                .resolve_strict_region(roi.x, roi.y, roi.width, roi.height)
            {
                Ok(rect) => Some(rect),
                Err(error) => return err_result(&format!("invalid OCR ROI: {error}")),
            },
            None => None,
        };

        let raw = match self.acquire_raw_capture(native_roi, p.window.clone()).await {
            Ok(raw) => raw,
            Err(e) => return err_result(&e),
        };

        // Encode once straight from the raw capture. The old path built an MCP
        // base64 screenshot and immediately decoded it back to JPEG here.
        let encode = tokio::task::spawn_blocking(move || {
            crate::capture::screenshot::encode_raw_capture(raw)
        });
        let encoded = match tokio::time::timeout(OCR_ENCODE_TIMEOUT, encode).await {
            Ok(Ok(Ok(encoded))) => encoded,
            Ok(Ok(Err(error))) => {
                return err_result(&format!("failed to encode OCR capture: {error}"));
            }
            Ok(Err(join_error)) => {
                return err_result(&format!("OCR encode task failed: {join_error}"));
            }
            Err(_) => return err_result("OCR JPEG encoding timed out after 10s"),
        };

        let encoded_view = encoded.view;
        let encoded_target_pid = encoded.target_pid;
        let (jpeg, w, h) = (encoded.jpeg, encoded.width, encoded.height);
        let languages = p
            .languages
            .clone()
            .unwrap_or_else(|| vec!["zh-Hans".to_string(), "en-US".to_string()]);
        let mode: crate::platform::OcrMode = p.mode.unwrap_or(OcrModeParam::Auto).into();

        // Bound admission as well as execution. Most importantly, move the
        // owned permit into the blocking closure: Tokio cannot cancel Apple's
        // synchronous Vision call, so an outer timeout must not release the
        // slot while that call is still consuming resources in the background.
        let permit = match tokio::time::timeout(OCR_QUEUE_TIMEOUT, ocr_gate().acquire_owned()).await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => return err_result(&format!("OCR concurrency gate closed: {error}")),
            Err(_) => {
                return err_result(
                    "OCR is busy: both recognition slots remained occupied for 5s; retry shortly",
                );
            }
        };

        // Recognition is blocking; run it off the async runtime with a mode-
        // aware hard timeout so a stuck recognizer cannot starve the server.
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let lang_refs: Vec<&str> = languages.iter().map(String::as_str).collect();
            crate::platform::ocr().recognize_with_mode(&jpeg, w, h, &lang_refs, mode)
        });
        let run_timeout = ocr_run_timeout(mode);
        let lines = match tokio::time::timeout(run_timeout, task).await {
            Ok(Ok(Ok(lines))) => lines,
            Ok(Ok(Err(e))) => return err_result(&format!("OCR failed: {e}")),
            Ok(Err(join_err)) => return err_result(&format!("OCR task failed: {join_err}")),
            Err(_) => {
                return err_result(&format!(
                    "OCR mode={} timed out after {run_timeout:?}; its bounded worker slot will \
                     remain reserved until the synchronous platform recognizer exits",
                    mode.as_str()
                ));
            }
        };

        // Commit the new coordinate frame only after recognition succeeds. A
        // busy gate, timeout, or platform error must not silently replace the
        // current view with an image/ROI the caller never received coordinates
        // for.
        self.set_view(encoded_view);
        if p.roi.is_some() {
            // A ROI refines the existing view; preserve its input target, just
            // like zoom_region does.
        } else if p.window.is_some() {
            self.set_target_pid(encoded_target_pid);
        } else {
            self.set_target_pid(None);
        }

        let subject = match (&p.window, &p.roi) {
            (Some(q), _) => format!("window matching {q:?}"),
            (_, Some(roi)) => format!(
                "ROI ({}, {}, {}, {}) of the previous view",
                roi.x, roi.y, roi.width, roi.height
            ),
            _ => "the main display".to_string(),
        };
        if lines.is_empty() {
            return ok_text(format!(
                "OCR of {subject} ({w}x{h} px, mode={}): no text recognized.",
                mode.as_str()
            ));
        }
        let mut note = format!(
            "OCR of {subject} ({w}x{h} px, mode={mode}), {n} text lines. Coordinates are in this \
             OCR view's pixel space (now the current image space) — click a line by its center with \
             left_click(x, y, source=\"ocr_center\").\n",
            mode = mode.as_str(),
            n = lines.len(),
        );
        for (i, line) in lines.iter().enumerate() {
            note.push_str(&format!(
                "  [{}] {:?} — ({:.0}, {:.0}), confidence={:.2}\n",
                i + 1,
                line.text,
                line.center.0,
                line.center.1,
                line.confidence,
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
        description = "Last-resort grounded left click in the most recent capture's pixel space. \
                       Pass source=ocr_center when x/y came from focused OCR; otherwise \
                       source=visual_coordinate (the default) means a focused screenshot/zoom. \
                       The result reports that route."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn left_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().left_click_at(lx, ly, self.current_target(p.background)) {
            Ok(()) => ok_text(format!(
                "route={} left clicked at ({}, {})",
                p.source
                    .as_ref()
                    .map(CoordinateActionSource::as_route)
                    .unwrap_or("visual_coordinate"),
                p.x,
                p.y
            )),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "right_click",
        description = "Grounded right click in the most recent capture's pixel space. Pass \
                       source=ocr_center for an OCR-returned center; otherwise the reported route \
                       defaults to visual_coordinate."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn right_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().right_click_at(lx, ly, self.current_target(p.background)) {
            Ok(()) => ok_text(format!(
                "route={} right clicked at ({}, {})",
                p.source
                    .as_ref()
                    .map(CoordinateActionSource::as_route)
                    .unwrap_or("visual_coordinate"),
                p.x,
                p.y
            )),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "double_click",
        description = "Grounded double click in the most recent capture's pixel space. Pass \
                       source=ocr_center for an OCR-returned center; otherwise the reported route \
                       defaults to visual_coordinate."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn double_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = self.to_logical(p.x, p.y);
        match crate::platform::input().double_click_at(lx, ly, self.current_target(p.background)) {
            Ok(()) => ok_text(format!(
                "route={} double clicked at ({}, {})",
                p.source
                    .as_ref()
                    .map(CoordinateActionSource::as_route)
                    .unwrap_or("visual_coordinate"),
                p.x,
                p.y
            )),
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
                       round-trips. Coordinates are in screenshot space. Afterwards use ax_read \
                       for semantic outcomes or a screenshot only for visual outcomes."
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
        description = "Legacy query action through macOS Accessibility or Windows UI Automation — \
                       no coordinates, no cursor movement, works in the background \
                       (the app need not be frontmost). `query` is a case-insensitive substring of \
                       the element's accessibility role or label/title (e.g. \"Send\", \"Search\"). \
                       Ambiguous matches fail closed. Prefer fresh ax_read + ax_activate for exact, \
                       generation-safe actions."
    )]
    #[tracing::instrument(skip_all, fields(query = %p.query), level = "info")]
    async fn ax_click(
        &self,
        Parameters(p): Parameters<AxQueryParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no AX/UIA target app; focus/open one or pass it to ax_read");
        };
        let query = p.query;
        let deadline = std::time::Instant::now() + AX_ACTION_TIMEOUT;
        let task = tokio::task::spawn_blocking(move || {
            crate::platform::ui_tree().ax_click(pid, &query, deadline)
        });
        match tokio::time::timeout(AX_ACTION_TIMEOUT + std::time::Duration::from_secs(1), task)
            .await
        {
            Ok(Ok(Ok(message))) => ok_text(message),
            Ok(Ok(Err(error))) => err_result(&error),
            Ok(Err(join_error)) => err_result(&format!("ax_click task failed: {join_error}")),
            Err(_) => err_result("ax_click exceeded its bounded action deadline"),
        }
    }

    #[tool(
        name = "ax_set_value",
        description = "Legacy query action that sets a control value through macOS Accessibility \
                       or Windows UI Automation. Background, no cursor. Ambiguous substring matches \
                       fail closed; prefer a fresh ax_read node whenever possible."
    )]
    #[tracing::instrument(skip_all, fields(query = %p.query), level = "info")]
    async fn ax_set_value(
        &self,
        Parameters(p): Parameters<AxSetValueParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no AX/UIA target app; focus/open one or pass it to ax_read");
        };
        let query = p.query;
        let value = p.value;
        let deadline = std::time::Instant::now() + AX_ACTION_TIMEOUT;
        let task = tokio::task::spawn_blocking(move || {
            crate::platform::ui_tree().ax_set_value(pid, &query, &value, deadline)
        });
        match tokio::time::timeout(AX_ACTION_TIMEOUT + std::time::Duration::from_secs(1), task)
            .await
        {
            Ok(Ok(Ok(message))) => ok_text(message),
            Ok(Ok(Err(error))) => err_result(&error),
            Ok(Err(join_error)) => err_result(&format!("ax_set_value task failed: {join_error}")),
            Err(_) => err_result("ax_set_value exceeded its bounded action deadline"),
        }
    }

    #[tool(
        name = "ax_focus",
        description = "Legacy query action that focuses a control through macOS Accessibility or \
                       Windows UI Automation. Background, no cursor. Ambiguous substring matches \
                       fail closed; prefer a fresh ax_read node whenever possible."
    )]
    #[tracing::instrument(skip_all, fields(query = %p.query), level = "info")]
    async fn ax_focus(
        &self,
        Parameters(p): Parameters<AxQueryParams>,
    ) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no AX/UIA target app; focus/open one or pass it to ax_read");
        };
        let query = p.query;
        let deadline = std::time::Instant::now() + AX_ACTION_TIMEOUT;
        let task = tokio::task::spawn_blocking(move || {
            crate::platform::ui_tree().ax_focus(pid, &query, deadline)
        });
        match tokio::time::timeout(AX_ACTION_TIMEOUT + std::time::Duration::from_secs(1), task)
            .await
        {
            Ok(Ok(Ok(message))) => ok_text(message),
            Ok(Ok(Err(error))) => err_result(&error),
            Ok(Err(join_error)) => err_result(&format!("ax_focus task failed: {join_error}")),
            Err(_) => err_result("ax_focus exceeded its bounded action deadline"),
        }
    }

    #[tool(
        name = "click_mark",
        description = "Compatibility activation by the mark NUMBER shown by the most recent \
                       ax_read/read_ui or screenshot(marks=true). Prefer ax_activate with \
                       snapshot_id/node_id because it rejects stale generations. Web-page content \
                       in a scriptable browser (Safari, Chrome, Arc, Edge, Brave, …) is clicked \
                       through the page's OWN JavaScript engine (an Accessibility press is a silent \
                       no-op on web content), native controls through the Accessibility tree; if \
                       neither applies it falls back to a click at the element's center. Numbers go \
                       stale when the UI changes, so run a fresh ax_read immediately before acting."
    )]
    #[tracing::instrument(skip_all, fields(number = %p.number, background = %p.background), level = "info")]
    async fn click_mark(
        &self,
        Parameters(p): Parameters<ClickMarkParams>,
    ) -> rmcp::model::CallToolResult {
        let target = self.current_target(p.background);
        let server = self.clone();
        let number = p.number;
        let deadline = std::time::Instant::now() + AX_ACTION_TIMEOUT;
        // Validation + token consumption are atomic inside the blocking task;
        // provider dispatch runs after releasing the short coordination gate.
        let task =
            tokio::task::spawn_blocking(move || server.activate_mark(number, target, deadline));
        match tokio::time::timeout(AX_ACTION_TIMEOUT + std::time::Duration::from_secs(1), task)
            .await
        {
            Ok(Ok(Ok(message))) => ok_text(message),
            Ok(Ok(Err(error))) => err_result(&error),
            Ok(Err(join_error)) => err_result(&format!("click_mark task failed: {join_error}")),
            Err(_) => err_result("click_mark exceeded its bounded action deadline"),
        }
    }

    #[tool(
        name = "ax_activate",
        description = "Activate one actionable node from the immediately preceding ax_read using \
                       its snapshot_id and node_id. This generation-safe action fails closed when \
                       the UI has been read again. It uses the web DOM bridge for scriptable \
                       browser content, native AX/UIA activation otherwise, then a freshly \
                       verified element-center fallback. \
                       The result reports route=ax|uia|web_dom|element_center. Every activation \
                       attempt consumes that generation before provider dispatch, so rerun \
                       ax_read after any result."
    )]
    #[tracing::instrument(skip_all, fields(snapshot_id = %p.snapshot_id, node_id = %p.node_id), level = "info")]
    async fn ax_activate(
        &self,
        Parameters(p): Parameters<AxActivateParams>,
    ) -> rmcp::model::CallToolResult {
        let target = self.current_target(p.background);
        let server = self.clone();
        let snapshot_id = p.snapshot_id;
        let node_id = p.node_id;
        let deadline = std::time::Instant::now() + AX_ACTION_TIMEOUT;
        let task = tokio::task::spawn_blocking(move || {
            server.activate_ax_node(&snapshot_id, &node_id, target, deadline)
        });
        match tokio::time::timeout(AX_ACTION_TIMEOUT + std::time::Duration::from_secs(1), task)
            .await
        {
            Ok(Ok(Ok(message))) => ok_text(message),
            Ok(Ok(Err(error))) => err_result(&error),
            Ok(Err(join_error)) => err_result(&format!("ax_activate task failed: {join_error}")),
            Err(_) => err_result("ax_activate exceeded its bounded action deadline"),
        }
    }

    #[tool(
        name = "dump_ax",
        description = "DEBUG: dump the target app's Accessibility tree (roles, subroles, labels, \
                       actions, frames) as indented text — to diagnose why some elements are not \
                       exposed. Targets the current AX/UIA app (or frontmost)."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn dump_ax(&self) -> rmcp::model::CallToolResult {
        let Some(pid) = self.current_ax_pid().await else {
            return err_result("no AX/UIA target app; focus/open one or call ax_read(window=...)");
        };
        match tokio::task::spawn_blocking(move || crate::platform::ui_tree().dump_tree(pid, 2500))
            .await
        {
            Ok(text) => ok_text(text),
            Err(join_err) => err_result(&format!("dump_ax task failed: {join_err}")),
        }
    }

    #[tool(
        name = "ax_read",
        description = "Canonical `ax:read` capability. Read visible semantic UI directly from \
                       macOS Accessibility or Windows UI Automation without taking a screenshot. \
                       Returns an ephemeral snapshot_id, deterministic node ids, role/name/\
                       description/value/actions/state, optional global-logical bounds, explicit \
                       coverage/fallback status, and actionable marks. mode=all (default) combines \
                       controls with labels/static text/headings; interactive or content narrows \
                       the view. Call ax_activate(snapshot_id,node_id) on a fresh actionable node. \
                       If coverage is absent/partial, use focused OCR for missing rendered text, \
                       then screenshot/zoom only for visual-only pixels. permission_denied means \
                       grant Accessibility; do not hide it with a screenshot fallback."
    )]
    #[tracing::instrument(skip_all, fields(window = ?p.window, max = ?p.max, mode = ?p.mode), level = "info")]
    async fn ax_read(
        &self,
        Parameters(p): Parameters<ReadUiParams>,
    ) -> rmcp::model::CallToolResult {
        self.run_ax_read(p).await
    }

    #[tool(
        name = "read_ui",
        description = "Compatibility alias for ax_read, backed by the exact same semantic \
                       traversal, renderer, snapshot generation, and action cache. Prefer the \
                       canonical ax_read name in new prompts and integrations."
    )]
    #[tracing::instrument(skip_all, fields(window = ?p.window, max = ?p.max, mode = ?p.mode), level = "info")]
    async fn read_ui(
        &self,
        Parameters(p): Parameters<ReadUiParams>,
    ) -> rmcp::model::CallToolResult {
        self.run_ax_read(p).await
    }

    #[tool(
        name = "chrome_status",
        description = "Report whether Nova.app's Chrome Native Messaging host is connected and whether one exact top-level document is currently paired. Call this before Chrome semantic work; it never inspects page content."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn chrome_status(&self) -> rmcp::model::CallToolResult {
        self.run_chrome_action(|bridge| bridge.status()).await
    }

    #[tool(
        name = "chrome_pair",
        description = "Begin an explicit 30-second pairing request for the active Chrome page. The user must inspect the origin and click Pair in the Nova extension popup. Success binds the exact tab, documentId, page nonce, and new epoch; it never grants access to another tab or later navigation."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn chrome_pair(&self) -> rmcp::model::CallToolResult {
        self.run_chrome_action(|bridge| bridge.pair()).await
    }

    #[tool(
        name = "chrome_read",
        description = "Read a bounded semantic DOM snapshot from the exact paired Chrome document. Sensitive fields and invalid/presentation ARIA nodes are excluded. Returns an ephemeral snapshotId and exact nodeIds; run this immediately before every Chrome mutation. No screenshot or coordinates are used."
    )]
    #[tracing::instrument(skip_all, fields(max_nodes = ?p.max_nodes, max_chars = ?p.max_chars), level = "info")]
    async fn chrome_read(
        &self,
        Parameters(p): Parameters<ChromeReadParams>,
    ) -> rmcp::model::CallToolResult {
        self.run_chrome_action(move |bridge| bridge.read(p.max_nodes, p.max_chars))
            .await
    }

    #[tool(
        name = "chrome_activate",
        description = "Semantically activate one exact actionable node from the immediately preceding chrome_read snapshot. Fails closed on stale snapshot, route, capability, or DOM identity; coordinate fallback is forbidden."
    )]
    #[tracing::instrument(skip_all, fields(snapshot_id = %p.snapshot_id, node_id = %p.node_id), level = "info")]
    async fn chrome_activate(
        &self,
        Parameters(p): Parameters<ChromeNodeParams>,
    ) -> rmcp::model::CallToolResult {
        self.run_chrome_action(move |bridge| bridge.activate(&p.snapshot_id, &p.node_id))
            .await
    }

    #[tool(
        name = "chrome_focus",
        description = "Focus one exact focus-capable node from the immediately preceding chrome_read snapshot. Fails closed instead of guessing or clicking."
    )]
    #[tracing::instrument(skip_all, fields(snapshot_id = %p.snapshot_id, node_id = %p.node_id), level = "info")]
    async fn chrome_focus(
        &self,
        Parameters(p): Parameters<ChromeNodeParams>,
    ) -> rmcp::model::CallToolResult {
        self.run_chrome_action(move |bridge| bridge.focus(&p.snapshot_id, &p.node_id))
            .await
    }

    #[tool(
        name = "chrome_set_value",
        description = "Set one exact non-sensitive text control from the immediately preceding chrome_read snapshot. Password, payment, OTP, file, hidden, and explicitly sensitive controls are unavailable. Plaintext is never logged or echoed; success returns only UTF-8 byte length and SHA-256."
    )]
    #[tracing::instrument(skip_all, fields(snapshot_id = %p.snapshot_id, node_id = %p.node_id), level = "info")]
    async fn chrome_set_value(
        &self,
        Parameters(p): Parameters<ChromeSetValueParams>,
    ) -> rmcp::model::CallToolResult {
        self.run_chrome_action(move |bridge| bridge.set_value(&p.snapshot_id, &p.node_id, &p.value))
            .await
    }

    #[tool(
        name = "chrome_scroll",
        description = "Semantically scroll one exact scroll-capable node (including root when exposed) from the immediately preceding chrome_read snapshot. Direction is up/down/left/right and amount is line/half_page/page; no coordinates."
    )]
    #[tracing::instrument(skip_all, fields(snapshot_id = %p.snapshot_id, node_id = %p.node_id, direction = ?p.direction, amount = ?p.amount), level = "info")]
    async fn chrome_scroll(
        &self,
        Parameters(p): Parameters<ChromeScrollParams>,
    ) -> rmcp::model::CallToolResult {
        let direction = p.direction.as_str();
        let amount = p.amount.as_str();
        self.run_chrome_action(move |bridge| {
            bridge.scroll(&p.snapshot_id, &p.node_id, direction, amount)
        })
        .await
    }

    #[tool(
        name = "chrome_release",
        description = "Explicitly release the currently paired Chrome document, revoke its epoch, in-flight requests, and unexpired receipts."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn chrome_release(&self) -> rmcp::model::CallToolResult {
        self.run_chrome_action(|bridge| bridge.release()).await
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

/// Run one MCP session over an authenticated Unix stream accepted by the
/// app-owned service.  The listener and peer-credential policy live in
/// `app_service`; this function only adapts the byte stream to rmcp's NDJSON
/// async read/write transport.
#[cfg(unix)]
pub async fn run_unix_stream(stream: tokio::net::UnixStream) -> Result<()> {
    run_unix_stream_with_server(stream, NovaServer::new()).await
}

/// Run one app-service MCP session with the app-owned Secure Chrome Bridge.
/// The bridge is injected explicitly so ordinary transports cannot acquire its
/// pairing authority by merely discovering the native-host socket.
#[cfg(unix)]
pub async fn run_unix_stream_with_chrome(
    stream: tokio::net::UnixStream,
    chrome_bridge: nova_chrome_bridge::ChromeBridge,
) -> Result<()> {
    run_unix_stream_with_server(stream, NovaServer::new().with_chrome_bridge(chrome_bridge)).await
}

#[cfg(unix)]
async fn run_unix_stream_with_server(
    stream: tokio::net::UnixStream,
    server: NovaServer,
) -> Result<()> {
    let (reader, writer) = stream.into_split();
    let service = server
        .serve((reader, writer))
        .await
        .context("app-service MCP session failed to initialize")?;

    let quit_reason = service
        .waiting()
        .await
        .context("app-service MCP session error")?;
    tracing::info!(?quit_reason, "Nova app-service MCP session stopped");
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
            "ax_activate",
            "ax_read",
            "read_ui",
            "dump_ax",
            "chrome_status",
            "chrome_pair",
            "chrome_read",
            "chrome_activate",
            "chrome_focus",
            "chrome_set_value",
            "chrome_scroll",
            "chrome_release",
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
    fn chrome_terminal_status_controls_the_mcp_error_flag() {
        for (status, expected_error) in [("ok", false), ("error", true), ("ambiguous", true)] {
            let result = chrome_call_result(Ok(serde_json::json!({
                "kind": "result",
                "status": status,
                "requestId": "app-1",
            })));
            assert_eq!(
                result.is_error,
                Some(expected_error),
                "unexpected MCP failure bit for Chrome status {status}"
            );
            let text = result.content[0].as_text().expect("Chrome JSON text");
            assert!(text.text.contains(&format!("\"status\": \"{status}\"")));
        }

        let malformed = chrome_call_result(Ok(serde_json::json!({ "kind": "result" })));
        assert_eq!(malformed.is_error, Some(true));
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

    // ── read_ui text listing (hermetic — no display / AX) ───────────────

    fn ui_line(number: u32, role: &str, label: &str, value: &str) -> UiLine {
        UiLine {
            number,
            role: role.to_string(),
            label: label.to_string(),
            value: value.to_string(),
        }
    }

    fn ax_line(
        node_id: &str,
        role: &str,
        name: &str,
        value: crate::platform::UiNodeValue,
        bounds: Option<crate::platform::UiBounds>,
    ) -> AxLine {
        AxLine {
            node_id: node_id.to_string(),
            mark: None,
            node: crate::platform::UiNode {
                role: role.to_string(),
                name: name.to_string(),
                description: String::new(),
                value,
                actions: Vec::new(),
                states: crate::platform::UiNodeStates::default(),
                bounds,
                depth: 1,
                actionable: false,
            },
        }
    }

    #[test]
    fn read_ui_listing_uses_the_same_mark_token_as_screenshots() {
        // The `[N] role "label"` shape must match `format_marks` so click_mark's
        // numbering contract is identical across read_ui and screenshot(marks).
        let lines = vec![
            ui_line(1, "AXButton", "Send", ""),
            ui_line(2, "AXLink", "Home", ""),
        ];
        let out = format_ui_listing(&lines, "window matching \"Mail\"", None);
        assert!(out.contains("[1] AXButton \"Send\""), "{out}");
        assert!(out.contains("[2] AXLink \"Home\""), "{out}");
        assert!(out.contains("click_mark"), "{out}");
        assert!(out.contains("2 actionable elements"), "{out}");
    }

    #[test]
    fn read_ui_listing_shows_field_value() {
        let lines = vec![ui_line(1, "AXTextField", "Search", "hello world")];
        let out = format_ui_listing(&lines, "the frontmost app", None);
        assert!(
            out.contains("[1] AXTextField \"Search\" = \"hello world\""),
            "{out}"
        );
    }

    #[test]
    fn read_ui_listing_empty_points_to_alternatives() {
        let out = format_ui_listing(&[], "the frontmost app", None);
        assert!(out.contains("no actionable elements"), "{out}");
        // Steers the model to the fallbacks rather than dead-ending.
        assert!(out.contains("screenshot") && out.contains("ocr"), "{out}");
    }

    #[test]
    fn read_ui_filter_narrows_display_but_keeps_original_numbers() {
        // Filtering is display-only: hidden elements keep their numbers so
        // click_mark(N) still resolves against the full cache.
        let lines = vec![
            ui_line(1, "AXButton", "Send", ""),
            ui_line(2, "AXTextField", "Search", ""),
            ui_line(3, "AXButton", "Search again", ""),
        ];
        let out = format_ui_listing(&lines, "the frontmost app", Some("search"));
        assert!(out.contains("2 of 3 actionable elements"), "{out}");
        // Matches keep their ORIGINAL numbers ([2], [3]); [1] is hidden.
        assert!(out.contains("[2] AXTextField \"Search\""), "{out}");
        assert!(out.contains("[3] AXButton \"Search again\""), "{out}");
        assert!(!out.contains("[1] AXButton \"Send\""), "{out}");
    }

    #[test]
    fn read_ui_filter_no_match_is_explicit() {
        let lines = vec![ui_line(1, "AXButton", "Send", "")];
        let out = format_ui_listing(&lines, "the frontmost app", Some("zzz"));
        assert!(out.contains("none of 1 actionable elements match"), "{out}");
    }

    #[test]
    fn read_ui_value_is_truncated_and_single_line() {
        // Value carries every kind of vertical whitespace a control might hold.
        let long = format!("{}\nsecond\rthird\tfourth", "x".repeat(200));
        let lines = vec![ui_line(1, "AXTextArea", "Body", &long)];
        let out = format_ui_listing(&lines, "the frontmost app", None);
        assert!(out.contains('…'), "long value should be truncated: {out}");
        // Newlines/carriage-returns/tabs in the value must not break the
        // one-element-per-line layout: only the leading list-separator newline
        // remains, and no stray \r/\t leak through.
        assert_eq!(out.matches('\n').count(), 1, "{out}");
        assert!(!out.contains('\r') && !out.contains('\t'), "{out}");
    }

    #[test]
    fn truncate_value_preserves_short_multibyte_text() {
        assert_eq!(truncate_value("héllo 中文"), "héllo 中文");
        let long = "中".repeat(100);
        let out = truncate_value(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 81); // 80 kept + ellipsis
    }

    #[test]
    fn ax_renderer_preserves_same_frame_text_and_never_prints_a_secret() {
        let shared_bounds = Some(crate::platform::UiBounds {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 24.0,
        });
        let lines = vec![
            ax_line(
                "n1",
                "AXStaticText",
                "first",
                crate::platform::UiNodeValue::Text("alpha".to_string()),
                shared_bounds,
            ),
            ax_line(
                "n2",
                "AXStaticText",
                "second",
                crate::platform::UiNodeValue::Text("beta".to_string()),
                shared_bounds,
            ),
            ax_line(
                "n3",
                "AXSecureTextField",
                "Password",
                crate::platform::UiNodeValue::Redacted,
                shared_bounds,
            ),
        ];
        let target = crate::platform::UiTarget {
            pid: 42,
            app_name: "Example".to_string(),
            window_title: "Login".to_string(),
            window_id: Some(7),
            bounds: shared_bounds,
        };
        let built = BuiltAxEntries {
            target,
            coverage: crate::platform::UiReadCoverage::Complete,
            truncated: false,
            partial_reason: None,
            cached: Vec::new(),
            lines,
        };
        let output = format_ax_snapshot(
            "ax-test-1",
            &built,
            crate::platform::UiReadMode::All,
            None,
            20_000,
        );
        assert!(
            output.contains("[n1]") && output.contains("alpha"),
            "{output}"
        );
        assert!(
            output.contains("[n2]") && output.contains("beta"),
            "{output}"
        );
        assert!(
            output.contains("[n3]") && output.contains("[REDACTED]"),
            "{output}"
        );
        assert!(!output.contains("hunter2"), "{output}");
    }

    #[test]
    fn ax_filter_only_changes_rendering_not_snapshot_local_ids() {
        let lines = vec![
            ax_line(
                "n1",
                "AXButton",
                "Cancel",
                crate::platform::UiNodeValue::Absent,
                None,
            ),
            ax_line(
                "n2",
                "AXButton",
                "Save",
                crate::platform::UiNodeValue::Absent,
                None,
            ),
        ];
        let target = crate::platform::UiTarget {
            pid: 1,
            app_name: "Example".to_string(),
            window_title: String::new(),
            window_id: None,
            bounds: None,
        };
        let built = BuiltAxEntries {
            target,
            coverage: crate::platform::UiReadCoverage::Complete,
            truncated: false,
            partial_reason: None,
            cached: Vec::new(),
            lines,
        };
        let output = format_ax_snapshot(
            "ax-test-2",
            &built,
            crate::platform::UiReadMode::All,
            Some("save"),
            20_000,
        );
        assert!(!output.contains("[n1]"), "{output}");
        assert!(output.contains("[n2]"), "{output}");
        assert!(output.contains("shown=1/2"), "{output}");
    }

    #[derive(Debug, Clone)]
    struct FakeElementHandle;

    impl crate::platform::ElementHandle for FakeElementHandle {
        fn click(&self) -> Result<&'static str, String> {
            Ok("FakeInvoke")
        }

        fn is_alive(&self) -> bool {
            true
        }

        fn current_center(&self) -> Option<(f64, f64)> {
            Some((5.0, 5.0))
        }

        fn try_web_click(
            &self,
            _pid: i32,
            _label: &str,
            _deadline: std::time::Instant,
        ) -> Option<Result<String, String>> {
            None
        }

        fn clone_box(&self) -> Box<dyn crate::platform::ElementHandle> {
            Box::new(self.clone())
        }
    }

    #[derive(Debug, Clone)]
    struct FailingFakeElementHandle;

    impl crate::platform::ElementHandle for FailingFakeElementHandle {
        fn prepare_for_action(&self, _deadline: std::time::Instant) -> Result<(), String> {
            Err("provider unavailable".to_string())
        }

        fn click(&self) -> Result<&'static str, String> {
            panic!("prepare_for_action must fail before click")
        }

        fn is_alive(&self) -> bool {
            true
        }

        fn current_center(&self) -> Option<(f64, f64)> {
            None
        }

        fn try_web_click(
            &self,
            _pid: i32,
            _label: &str,
            _deadline: std::time::Instant,
        ) -> Option<Result<String, String>> {
            None
        }

        fn clone_box(&self) -> Box<dyn crate::platform::ElementHandle> {
            Box::new(self.clone())
        }
    }

    fn fake_cached(number: u32) -> crate::tools::elements::CachedElement {
        crate::tools::elements::CachedElement {
            number,
            handle: Box::new(FakeElementHandle),
            center: (5.0, 5.0),
            role: "Button".to_string(),
            label: "Save".to_string(),
            pid: 42,
        }
    }

    #[test]
    fn semantic_activation_does_not_require_coordinate_bounds() {
        let node = crate::platform::UiNode {
            role: "Button".to_string(),
            name: "Save".to_string(),
            description: String::new(),
            value: crate::platform::UiNodeValue::Absent,
            actions: vec!["Invoke".to_string()],
            states: crate::platform::UiNodeStates::default(),
            bounds: None,
            depth: 1,
            actionable: true,
        };
        let snapshot = crate::platform::UiSnapshot {
            target: crate::platform::UiTarget {
                pid: 42,
                app_name: "Example".to_string(),
                window_title: "Document".to_string(),
                window_id: None,
                bounds: None,
            },
            nodes: vec![crate::platform::CollectedUiNode {
                node,
                handle: Some(Box::new(FakeElementHandle)),
            }],
            coverage: crate::platform::UiReadCoverage::Complete,
            truncated: false,
            partial_reason: None,
        };
        let built = build_ax_entries(snapshot);
        assert_eq!(built.lines[0].mark, Some(1));
        assert_eq!(built.cached.len(), 1);
    }

    #[test]
    fn ax_activation_rejects_a_stale_snapshot_generation() {
        let server = NovaServer::new();
        let line = AxLine {
            node_id: "n1".to_string(),
            mark: Some(1),
            node: crate::platform::UiNode {
                role: "Button".to_string(),
                name: "Save".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: vec!["Invoke".to_string()],
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 1,
                actionable: true,
            },
        };
        let old = server.set_ax_nodes(
            vec![("n1".to_string(), fake_cached(1))],
            std::slice::from_ref(&line),
        );
        assert!(server.get_ax_node(&old, "n1").is_ok());
        let current = server.set_ax_nodes(vec![("n1".to_string(), fake_cached(1))], &[line]);
        let error = server.get_ax_node(&old, "n1").expect_err("old generation");
        assert!(error.contains("stale snapshot"), "{error}");
        assert!(server.get_ax_node(&current, "n1").is_ok());
    }

    #[test]
    fn activation_attempt_consumes_its_generation() {
        let server = NovaServer::new();
        let line = AxLine {
            node_id: "n1".to_string(),
            mark: Some(1),
            node: crate::platform::UiNode {
                role: "Button".to_string(),
                name: "Save".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: vec!["Invoke".to_string()],
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 1,
                actionable: true,
            },
        };
        let generation = server.set_ax_nodes(vec![("n1".to_string(), fake_cached(1))], &[line]);
        let result = server.activate_ax_node(
            &generation,
            "n1",
            crate::tools::input::InputTarget::Global,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        assert!(result.is_ok(), "{result:?}");
        let error = server
            .get_ax_node(&generation, "n1")
            .expect_err("action attempt must consume its generation");
        assert!(error.contains("stale snapshot"), "{error}");
    }

    #[test]
    fn failed_activation_attempt_also_consumes_its_generation() {
        let server = NovaServer::new();
        let line = AxLine {
            node_id: "n1".to_string(),
            mark: Some(1),
            node: crate::platform::UiNode {
                role: "Button".to_string(),
                name: "Save".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: vec!["Invoke".to_string()],
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 1,
                actionable: true,
            },
        };
        let cached = crate::tools::elements::CachedElement {
            number: 1,
            handle: Box::new(FailingFakeElementHandle),
            center: (0.0, 0.0),
            role: "Button".to_string(),
            label: "Save".to_string(),
            pid: 42,
        };
        let generation = server.set_ax_nodes(
            vec![("n1".to_string(), cached)],
            std::slice::from_ref(&line),
        );

        let error = server
            .activate_ax_node(
                &generation,
                "n1",
                crate::tools::input::InputTarget::Global,
                std::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .expect_err("provider failure must surface");
        assert!(error.contains("provider unavailable"), "{error}");
        assert!(error.contains("consumed before dispatch"), "{error}");
        let stale = server
            .get_ax_node(&generation, "n1")
            .expect_err("failed action attempt must consume its generation");
        assert!(stale.contains("stale snapshot"), "{stale}");
    }

    #[tokio::test]
    async fn every_ax_read_attempt_invalidates_the_previous_generation() {
        let server = NovaServer::new();
        let line = AxLine {
            node_id: "n1".to_string(),
            mark: Some(1),
            node: crate::platform::UiNode {
                role: "Button".to_string(),
                name: "Save".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: vec!["Invoke".to_string()],
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 1,
                actionable: true,
            },
        };
        let old = server.set_ax_nodes(
            vec![("n1".to_string(), fake_cached(1))],
            std::slice::from_ref(&line),
        );
        let result = server
            .ax_read(Parameters(ReadUiParams {
                window: None,
                filter: None,
                max: None,
                mode: Some("not-a-mode".to_string()),
                max_chars: None,
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
        let error = server
            .get_ax_node(&old, "n1")
            .expect_err("failed read must invalidate the old generation");
        assert!(error.contains("stale snapshot"), "{error}");
    }

    #[derive(Debug, Clone)]
    struct BlockingFakeElementHandle {
        entered: std::sync::Arc<std::sync::Barrier>,
        release: std::sync::Arc<std::sync::Barrier>,
    }

    impl crate::platform::ElementHandle for BlockingFakeElementHandle {
        fn click(&self) -> Result<&'static str, String> {
            self.entered.wait();
            self.release.wait();
            Ok("FakeInvoke")
        }

        fn is_alive(&self) -> bool {
            true
        }

        fn current_center(&self) -> Option<(f64, f64)> {
            None
        }

        fn try_web_click(
            &self,
            _pid: i32,
            _label: &str,
            _deadline: std::time::Instant,
        ) -> Option<Result<String, String>> {
            None
        }

        fn clone_box(&self) -> Box<dyn crate::platform::ElementHandle> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn provider_dispatch_does_not_hold_the_generation_gate() {
        let server = NovaServer::new();
        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let line = AxLine {
            node_id: "n1".to_string(),
            mark: Some(1),
            node: crate::platform::UiNode {
                role: "Button".to_string(),
                name: "Save".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: vec!["Invoke".to_string()],
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 1,
                actionable: true,
            },
        };
        let cached = crate::tools::elements::CachedElement {
            number: 1,
            handle: Box::new(BlockingFakeElementHandle {
                entered: entered.clone(),
                release: release.clone(),
            }),
            center: (0.0, 0.0),
            role: "Button".to_string(),
            label: "Save".to_string(),
            pid: 42,
        };
        let generation = server.set_ax_nodes(
            vec![("n1".to_string(), cached)],
            std::slice::from_ref(&line),
        );

        let actor = server.clone();
        let action_generation = generation.clone();
        let action = std::thread::spawn(move || {
            actor.activate_ax_node(
                &action_generation,
                "n1",
                crate::tools::input::InputTarget::Global,
                std::time::Instant::now() + std::time::Duration::from_secs(2),
            )
        });
        entered.wait();
        let stale = server.get_ax_node(&generation, "n1");

        let invalidator = server.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let invalidate = std::thread::spawn(move || {
            let next = invalidator.invalidate_interaction();
            sent.send(next).expect("report invalidation");
        });
        let next = received.recv_timeout(std::time::Duration::from_millis(250));
        release.wait();
        let action_result = action.join().expect("action thread");
        invalidate.join().expect("invalidation thread");
        let stale = stale.expect_err("token must be consumed before provider dispatch");
        assert!(stale.contains("stale snapshot"), "{stale}");
        let next = next.expect("generation replacement must not wait for provider dispatch");
        assert!(action_result.is_ok(), "{action_result:?}");
        assert_ne!(next, generation);
        assert!(server.get_ax_node(&generation, "n1").is_err());
    }

    #[test]
    fn non_actionable_node_reports_a_related_action_target_without_clicking() {
        let server = NovaServer::new();
        let content = AxLine {
            node_id: "n1".to_string(),
            mark: None,
            node: crate::platform::UiNode {
                role: "Group".to_string(),
                name: "Dialog".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: Vec::new(),
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 1,
                actionable: false,
            },
        };
        let action = AxLine {
            node_id: "n2".to_string(),
            mark: Some(1),
            node: crate::platform::UiNode {
                role: "Button".to_string(),
                name: "Confirm".to_string(),
                description: String::new(),
                value: crate::platform::UiNodeValue::Absent,
                actions: vec!["Invoke".to_string()],
                states: crate::platform::UiNodeStates::default(),
                bounds: None,
                depth: 2,
                actionable: true,
            },
        };
        let snapshot =
            server.set_ax_nodes(vec![("n2".to_string(), fake_cached(1))], &[content, action]);
        let error = server
            .get_ax_node(&snapshot, "n1")
            .expect_err("content node must not activate");
        assert!(error.contains("not actionable"), "{error}");
        assert!(error.contains("n2"), "{error}");
    }

    #[test]
    fn ax_renderer_enforces_a_unicode_character_budget_with_a_marker() {
        let target = crate::platform::UiTarget {
            pid: 1,
            app_name: "Example".to_string(),
            window_title: String::new(),
            window_id: None,
            bounds: None,
        };
        let built = BuiltAxEntries {
            target,
            coverage: crate::platform::UiReadCoverage::Complete,
            truncated: false,
            partial_reason: None,
            cached: Vec::new(),
            lines: vec![ax_line(
                "n1",
                "AXStaticText",
                "long",
                crate::platform::UiNodeValue::Text("中".repeat(4_000)),
                None,
            )],
        };
        let output = format_ax_snapshot(
            "ax-budget",
            &built,
            crate::platform::UiReadMode::All,
            None,
            4_096,
        );
        assert!(output.contains("truncated=true"), "{output}");
        assert!(
            output.contains("partial_reason=character_limit"),
            "{output}"
        );
        assert!(output.chars().count() <= 4_096);
    }

    #[test]
    fn ax_read_mode_is_explicit_and_fail_closed() {
        assert_eq!(
            parse_ax_read_mode(None).unwrap(),
            crate::platform::UiReadMode::All
        );
        assert_eq!(
            parse_ax_read_mode(Some("content")).unwrap(),
            crate::platform::UiReadMode::Content
        );
        assert!(parse_ax_read_mode(Some("visual")).is_err());
    }

    #[test]
    fn all_shipped_guidance_contains_the_same_ax_first_ladder() {
        let documents = [
            ("server instructions", NOVA_INSTRUCTIONS),
            (
                "plugin skill",
                include_str!("../packaging/plugin/skills/nova-grounding/SKILL.md"),
            ),
            (
                "plugin README",
                include_str!("../packaging/plugin/README.md"),
            ),
            ("root README", include_str!("../README.md")),
        ];
        for (name, document) in documents {
            let lower = document.to_lowercase();
            for required in ["ax_read", "ax_activate", "ocr", "screenshot"] {
                assert!(
                    lower.contains(required),
                    "{name} omitted required AX-first policy term {required:?}"
                );
            }
            assert!(
                lower.contains("consum") || lower.contains("single-use"),
                "{name} does not state that each activation attempt is single-use"
            );
            assert!(
                !lower.contains("take a screenshot after each"),
                "{name} contains the retired screenshot-after-every-action policy"
            );
            assert!(
                !lower.contains("take screenshot(marks=true)"),
                "{name} contains the retired screenshot-first policy"
            );
        }

        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../packaging/plugin/plugin.json"))
                .expect("plugin template JSON");
        let prompt = manifest["provides"]["prompts"][0]["content"]
            .as_str()
            .expect("nova_desktop prompt");
        for required in ["ax_read", "ax_activate", "ocr", "screenshot"] {
            assert!(prompt.to_lowercase().contains(required), "{prompt}");
        }
        assert!(
            prompt.to_lowercase().contains("consum"),
            "nova_desktop prompt omitted the single-use generation contract"
        );
    }

    #[test]
    fn mac_semantic_read_sources_have_no_capture_call_edge() {
        let sources = [
            include_str!("platform/mac/elements/target.rs"),
            include_str!("platform/mac/elements/semantic.rs"),
        ];
        for source in sources {
            for forbidden in [
                "crate::platform::screen_capture(",
                "crate::platform::window_manager(",
                "crate::tools::window::",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "ax_read acquired a capture-backed dependency: {forbidden}"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generated_manifest_preserves_the_reviewed_nova_desktop_prompt() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let output = std::env::temp_dir().join(format!(
            "nova-plugin-prompt-golden-{}.json",
            std::process::id()
        ));
        let status = std::process::Command::new("bash")
            .arg(root.join("packaging/plugin/generate-manifest.sh"))
            .arg("9.9.9")
            .arg("a".repeat(64))
            .arg("b".repeat(64))
            .arg(&output)
            .status()
            .expect("run manifest generator");
        assert!(status.success());
        let template: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("packaging/plugin/plugin.json"))
                .expect("read template"),
        )
        .expect("parse template");
        let generated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output).expect("read generated"))
                .expect("parse generated");
        let _ = std::fs::remove_file(&output);
        assert_eq!(
            generated["provides"]["prompts"], template["provides"]["prompts"],
            "release generation changed the reviewed nova_desktop prompt"
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[tokio::test]
    async fn ax_read_and_read_ui_alias_share_the_headless_typed_outcome() {
        fn params() -> ReadUiParams {
            ReadUiParams {
                window: None,
                filter: None,
                max: None,
                mode: None,
                max_chars: None,
            }
        }
        let server = NovaServer::new();
        let canonical = server.ax_read(Parameters(params())).await;
        let alias = server.read_ui(Parameters(params())).await;
        let canonical_text = canonical.content[0]
            .as_text()
            .expect("canonical text")
            .text
            .as_str();
        let alias_text = alias.content[0]
            .as_text()
            .expect("alias text")
            .text
            .as_str();
        assert!(canonical_text.contains("status=unsupported_platform"));
        assert_eq!(canonical_text, alias_text);
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

    #[tokio::test]
    async fn ocr_rejects_window_and_roi_before_capture() {
        let server = NovaServer::new();
        let result = server
            .ocr(Parameters(OcrParams {
                window: Some("Safari".to_string()),
                languages: None,
                mode: Some(OcrModeParam::Auto),
                roi: Some(OcrRoiParams {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                }),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
        let text = &result.content[0].as_text().expect("text error").text;
        assert!(text.contains("mutually exclusive"));
    }

    #[test]
    fn ocr_modes_deserialize_and_have_bounded_budgets() {
        let fast: OcrParams = serde_json::from_value(serde_json::json!({
            "mode": "fast"
        }))
        .expect("deserialize fast OCR mode");
        assert!(matches!(fast.mode, Some(OcrModeParam::Fast)));

        assert!(
            ocr_run_timeout(crate::platform::OcrMode::Fast)
                < ocr_run_timeout(crate::platform::OcrMode::Accurate)
        );
        assert!(
            ocr_run_timeout(crate::platform::OcrMode::Accurate)
                < ocr_run_timeout(crate::platform::OcrMode::Auto)
        );
        assert_eq!(OCR_MAX_CONCURRENT, 2);
    }
}
