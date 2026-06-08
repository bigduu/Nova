use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Nova — Computer Use MCP Server
///
/// A macOS desktop control MCP server that gives LLMs the ability to
/// capture screenshots, control mouse/keyboard, manage windows, and introspect apps.
#[derive(Parser, Debug)]
#[command(name = "nova", version, about)]
struct Cli {
    /// Run in Streamable HTTP mode (default: stdio)
    #[arg(long)]
    http: bool,

    /// HTTP listen address (default: 127.0.0.1:3100)
    #[arg(long, default_value = "127.0.0.1:3100")]
    addr: String,

    /// Self-test: do a direct capture (no MCP server) and print timing, then exit.
    #[arg(long)]
    selftest: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Init logging — MUST go to stderr. In stdio transport, stdout is the
    // JSON-RPC channel; any log line written there corrupts the protocol stream
    // and makes clients (e.g. when RUST_LOG is set) hang waiting for a response.
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let cli = Cli::parse();

    // Bootstrap the CoreGraphics / window-server connection BEFORE any
    // ScreenCaptureKit call. Without this, capture from this subprocess either
    // SIGABRTs (CGS_REQUIRE_INIT) or hangs in replayd-connection churn. See
    // `nova::capture::init_core_graphics`.
    nova::capture::init_core_graphics();

    tracing::info!(
        "Nova Computer Use MCP Server v{}",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!(
        "Transport: {}",
        if cli.http { "Streamable HTTP" } else { "stdio" }
    );

    if cli.selftest {
        // Probe the actual *capture* authorization (distinct from the content
        // enumeration that SCShareableContent::get checks).
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        let preflight = unsafe { CGPreflightScreenCaptureAccess() };
        eprintln!("[SELFTEST] CGPreflightScreenCaptureAccess() = {preflight}");
        eprintln!(
            "[SELFTEST] screen_recording_available() (via SCShareableContent::get) = {}",
            nova::display::geometry::screen_recording_available()
        );

        // SAME binary, no MCP server: capture directly on a blocking thread.
        let t = std::time::Instant::now();
        let h = tokio::task::spawn_blocking(|| {
            nova::capture::screenshot::capture_display()
        });
        match tokio::time::timeout(std::time::Duration::from_secs(20), h).await {
            Ok(Ok(Ok(img))) => {
                eprintln!(
                    "[SELFTEST] OK {}x{} in {:.0} ms",
                    img.width,
                    img.height,
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
            Ok(Ok(Err(e))) => eprintln!("[SELFTEST] capture error: {e}"),
            Ok(Err(e)) => eprintln!("[SELFTEST] join error: {e}"),
            Err(_) => eprintln!(
                "[SELFTEST] TIMED OUT after 20s ({:.0} ms)",
                t.elapsed().as_secs_f64() * 1000.0
            ),
        }
        return Ok(());
    }

    if cli.http {
        nova::server::run_http(&cli.addr).await
    } else {
        nova::server::run_stdio().await
    }
}
