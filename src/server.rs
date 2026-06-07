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

// ── Server state ────────────────────────────────────────────────────

/// Shared state for the Nova MCP server.
/// All tool handlers receive `&self` to this struct.
#[derive(Debug, Clone)]
pub struct NovaServer;

impl NovaServer {
    pub fn new() -> Self {
        NovaServer
    }
}

impl Default for NovaServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert screenshot-space coordinates (what the LLM sees) into the global
/// logical points that mouse events are posted in.
fn to_logical(x: f64, y: f64) -> (f64, f64) {
    crate::display::geometry::screen_to_logical_coords(x, y)
}

// ── Tool implementations ────────────────────────────────────────────

use rmcp::handler::server::wrapper::Parameters;
use rmcp::tool;
use rmcp::tool_router;
use serde::Deserialize;

// Tool parameter types — all stub, to be fleshed out in implementation.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MouseMoveParams {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
    pub x: f64,
    pub y: f64,
    pub lines: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KeyParams {
    pub key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeParams {
    pub text: String,
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

#[tool_router(server_handler)]
impl NovaServer {
    #[tool(
        name = "screenshot",
        description = "Take a screenshot of the primary display. Returns a base64-encoded JPEG image."
    )]
    #[tracing::instrument(skip_all, level = "info")]
    async fn screenshot(
        &self,
        Parameters(_p): Parameters<ScreenshotParams>,
    ) -> rmcp::model::CallToolResult {
        match crate::tools::screenshot::take_screenshot() {
            Ok(img) => ok_image(img.base64_data, img.mime_type),
            Err(e) => err_result(&e),
        }
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
        let (lx, ly) = to_logical(p.x, p.y);
        match crate::tools::input::mouse_move(lx, ly) {
            Ok(()) => ok_text(format!("mouse moved to ({}, {})", p.x, p.y)),
            Err(e) => err_result(&e.to_string()),
        }
    }

    #[tool(
        name = "left_click",
        description = "Left-click at the given (x, y) coordinates (in screenshot space)."
    )]
    #[tracing::instrument(skip_all, fields(x = %p.x, y = %p.y), level = "info")]
    async fn left_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> rmcp::model::CallToolResult {
        let (lx, ly) = to_logical(p.x, p.y);
        match crate::tools::input::left_click_at(lx, ly) {
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
        let (lx, ly) = to_logical(p.x, p.y);
        match crate::tools::input::right_click_at(lx, ly) {
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
        let (lx, ly) = to_logical(p.x, p.y);
        match crate::tools::input::mouse_move(lx, ly).and_then(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            crate::tools::input::double_click()
        }) {
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
        // Move to the requested position first so the scroll lands on the
        // intended region, not wherever the cursor happened to be.
        let (lx, ly) = to_logical(p.x, p.y);
        let result = crate::tools::input::mouse_move(lx, ly).and_then(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            crate::tools::input::scroll(p.lines)
        });
        match result {
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
        match crate::tools::input::key_combo(&p.key) {
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
        match crate::tools::input::type_text(&p.text) {
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
        match crate::tools::input::cursor_position() {
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
        match crate::tools::window::list_windows() {
            Ok(windows) => ok_text(serde_json::to_string_pretty(&windows).unwrap_or_default()),
            Err(e) => err_result(&e),
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
        match crate::tools::batch::execute_batch(p.actions).await {
            Ok(results) => ok_text(results.join("\n")),
            Err(e) => err_result(&e.to_string()),
        }
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
}
