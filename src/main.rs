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

    /// INTERNAL: run as the capture worker subprocess. Reads JSON capture
    /// requests on stdin and writes raw images on stdout; spawned by the server
    /// to isolate the hang-prone ScreenCaptureKit call. Not for direct use.
    #[arg(long, hide = true)]
    capture_worker: bool,

    /// DEBUG: dump the Accessibility tree of the app whose window/name matches
    /// this substring (e.g. "Arc"), then exit. No MCP needed. Stdout = the tree.
    #[arg(long, value_name = "APP")]
    dump_ax: Option<String>,

    /// DEBUG: list the Set-of-Mark actionable elements nova would mark for the
    /// app matching this substring, then exit. No MCP needed.
    #[arg(long, value_name = "APP")]
    marks: Option<String>,

    /// DEBUG: hit-test a grid over the content area of the app matching this
    /// substring and print, per distinct element, its role/actions and whether
    /// the actionable-ancestor climb accepts it. Shows WHY visible rows aren't
    /// marked. No MCP needed.
    #[arg(long, value_name = "APP")]
    hit_dump: Option<String>,

    /// DEBUG: in ONE process, enable web-AX then probe the app repeatedly over
    /// several rounds — tests whether a long-lived process stabilizes Chromium's
    /// full semantic tree (the "Homerow way"). No MCP needed.
    #[arg(long, value_name = "APP")]
    ax_warm: Option<String>,
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

    // Capture worker subprocess: just loop on stdin/stdout doing raw captures.
    // (Bootstrap above already ran, which is exactly what this child needs.)
    if cli.capture_worker {
        nova::capture::worker::run();
    }

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
        let h = tokio::task::spawn_blocking(nova::capture::screenshot::capture_display);
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

    // ── DEBUG CLI subcommands (no MCP) ──────────────────────────────
    if let Some(app) = cli.dump_ax.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, _frame)) => {
                eprintln!("[dump-ax] {app:?} -> pid {pid}");
                print!("{}", nova::tools::elements::dump_tree(pid, 4000));
            }
            None => eprintln!("[dump-ax] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }
    if let Some(app) = cli.marks.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, frame)) => {
                eprintln!("[marks] {app:?} -> pid {pid} clip={frame:?}");
                let els = nova::tools::elements::collect_actionable(pid, 400, Some(frame));
                eprintln!("[marks] {} actionable elements:", els.len());
                for (i, (el, _)) in els.iter().enumerate() {
                    println!(
                        "[{}] {} {:?} @({:.0},{:.0} {:.0}x{:.0})",
                        i + 1,
                        el.role,
                        el.label,
                        el.x,
                        el.y,
                        el.width,
                        el.height
                    );
                }
            }
            None => eprintln!("[marks] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }
    if let Some(app) = cli.hit_dump.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, frame)) => {
                eprintln!("[hit-dump] {app:?} -> pid {pid} clip={frame:?}");
                // Skip the left ~280px (native sidebar) so we probe just the
                // web/content region whose rows aren't getting marked.
                print!(
                    "{}",
                    nova::tools::elements::hit_dump(pid, frame, 24.0, 280.0)
                );
            }
            None => eprintln!("[hit-dump] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }
    if let Some(app) = cli.ax_warm.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, frame)) => {
                eprintln!("[ax-warm] {app:?} -> pid {pid} clip={frame:?}");
                print!("{}", nova::tools::elements::ax_warm_probe(pid, frame, 12));
            }
            None => eprintln!("[ax-warm] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }

    // Request Screen Recording access from THIS (server) process before serving —
    // it surfaces the first-run system prompt and is a no-op once granted. Done
    // here, not in the headless capture worker (which can't show a prompt).
    let screen_ok = nova::display::geometry::request_screen_recording_access();
    tracing::info!(
        "Screen Recording access: {}",
        if screen_ok {
            "granted"
        } else {
            "not granted — accept the prompt, or add the nova binary in System \
             Settings → Privacy & Security → Screen Recording"
        }
    );

    if cli.http {
        nova::server::run_http(&cli.addr).await
    } else {
        nova::server::run_stdio().await
    }
}
