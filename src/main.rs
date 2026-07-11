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

    /// INTERNAL: the SCK-touching half of --selftest, run in a SACRIFICIAL
    /// subprocess. Any process that touches ScreenCaptureKit becomes a replayd
    /// client that collides with the daemon (same-binary identity), so the
    /// direct-stream probe must die before the daemon probe runs.
    #[arg(long, hide = true)]
    selftest_direct: bool,

    /// INTERNAL: run as the shared per-user capture daemon. Owns the ONE
    /// ScreenCaptureKit client all nova processes route captures through (two
    /// same-binary processes holding replayd streams evict each other's XPC
    /// identity and wedge — see platform::mac::capture::broker). Spawned on demand; elected
    /// via a flock. Not for direct use.
    #[arg(long, hide = true)]
    capture_daemon: bool,

    /// INTERNAL (legacy): old pipe-protocol capture worker, kept as a thin
    /// proxy that forwards to the capture daemon — still-running nova servers
    /// from pre-daemon builds spawn this from the binary on disk. Not for
    /// direct use.
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
    // `nova::platform::mac::capture::init_core_graphics`.
    nova::platform::mac::capture::init_core_graphics();

    // Shared capture daemon: serve capture requests over the per-user socket.
    // (Bootstrap above already ran, which is exactly what this process needs.)
    //
    // These low-level `--capture-daemon`/`--capture-worker`/`--selftest*`
    // paths are diagnostics/plumbing for the capture daemon ITSELF, not
    // tool-layer logic — they call `platform::mac::capture` directly,
    // bypassing the `ScreenCapture` trait, same rationale as the debug CLI's
    // direct `tools::elements`/`tools::window` calls below.
    if cli.capture_daemon {
        nova::platform::mac::capture::broker::run_daemon();
    }
    // Legacy worker entry point: proxy the old pipe protocol into the daemon.
    if cli.capture_worker {
        nova::platform::mac::capture::broker::run_worker_proxy();
    }

    tracing::info!(
        "Nova Computer Use MCP Server v{}",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!(
        "Transport: {}",
        if cli.http { "Streamable HTTP" } else { "stdio" }
    );

    if cli.selftest_direct {
        // SCK-touching probes, isolated in this short-lived process. Our exit
        // closes the replayd XPC connection these open — leaving it open in the
        // main selftest process would wedge the daemon probe that follows.
        let probe =
            tokio::task::spawn_blocking(nova::platform::mac::geometry::screen_recording_available);
        match tokio::time::timeout(std::time::Duration::from_secs(5), probe).await {
            Ok(Ok(ok)) => eprintln!(
                "[SELFTEST] screen_recording_available() (via SCShareableContent::get) = {ok}"
            ),
            _ => eprintln!(
                "[SELFTEST] screen_recording_available() TIMED OUT after 5s — \
                 SCShareableContent itself is wedged"
            ),
        }
        let t = std::time::Instant::now();
        let h = tokio::task::spawn_blocking(|| {
            nova::platform::mac::capture::stream::StreamCapturer::new().capture_display()
        });
        match tokio::time::timeout(std::time::Duration::from_secs(10), h).await {
            Ok(Ok(Ok(raw))) => {
                eprintln!(
                    "[SELFTEST] direct stream: OK {}x{} in {:.0} ms",
                    raw.image.width(),
                    raw.image.height(),
                    t.elapsed().as_secs_f64() * 1000.0
                );
            }
            Ok(Ok(Err(e))) => eprintln!("[SELFTEST] direct stream: capture error: {e}"),
            Ok(Err(e)) => eprintln!("[SELFTEST] direct stream: join error: {e}"),
            Err(_) => eprintln!(
                "[SELFTEST] direct stream: TIMED OUT after 10s — another process is \
                 holding a ScreenCaptureKit stream (a live capture daemon is normal \
                 here; a wedge is not)"
            ),
        }
        // NOTE: process::exit, not return — Runtime::drop would WAIT for a
        // wedged spawn_blocking thread (uncancellable SCK condvar) and this
        // child would never exit; and only process death closes the replayd
        // connection the probes opened.
        std::process::exit(0);
    }

    if cli.selftest {
        // Probe the actual *capture* authorization (CoreGraphics TCC lookup —
        // does NOT touch replayd, so it can't contaminate the daemon probe).
        let preflight = nova::platform::mac::geometry::preflight_screen_capture();
        eprintln!("[SELFTEST] CGPreflightScreenCaptureAccess() = {preflight}");

        // Direct-path probes (SCShareableContent + a private StreamCapturer) in
        // a sacrificial subprocess: a process that has touched ScreenCaptureKit
        // keeps a replayd client connection that collides with the daemon's.
        // Only meaningful on a quiet system — with a live capture daemon the
        // probe is GUARANTEED to collide with the daemon's stream, so skip it.
        if let Some(pid) = nova::platform::mac::capture::broker::any_capture_daemon_pid() {
            eprintln!(
                "[SELFTEST] direct stream: skipped (capture daemon pid={pid} is live; \
                 its warm stream would collide with a second same-binary stream)"
            );
        } else {
            // Bounded wait + SIGKILL: even though the child exits via
            // process::exit after its own timeouts, the parent must never
            // hang on it (--selftest is the tool that diagnoses hangs).
            let exe = std::env::current_exe()?;
            match std::process::Command::new(&exe)
                .arg("--selftest-direct")
                .stdin(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
                    loop {
                        match child.try_wait() {
                            Ok(Some(s)) if !s.success() => {
                                eprintln!("[SELFTEST] direct probe exited: {s}");
                                break;
                            }
                            Ok(Some(_)) => break,
                            Ok(None) if std::time::Instant::now() >= deadline => {
                                let _ = child.kill();
                                let _ = child.wait();
                                eprintln!(
                                    "[SELFTEST] direct probe KILLED after 25s — it hung \
                                     past its own internal timeouts"
                                );
                                break;
                            }
                            Ok(None) => {
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await
                            }
                            Err(e) => {
                                eprintln!("[SELFTEST] direct probe wait failed: {e}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => eprintln!("[SELFTEST] direct probe failed to run: {e}"),
            }
        }

        // Daemon path: the one production actually uses (connect-or-spawn the
        // shared daemon, capture through it, full recovery ladder).
        let t = std::time::Instant::now();
        let h = tokio::task::spawn_blocking(|| {
            nova::platform::mac::capture::broker::shared_client()
                .capture(&nova::platform::mac::capture::broker::CaptureRequest::Display)
        });
        match tokio::time::timeout(std::time::Duration::from_secs(60), h).await {
            Ok(Ok(Ok(raw))) => eprintln!(
                "[SELFTEST] capture daemon: OK {}x{} in {:.0} ms",
                raw.image.width(),
                raw.image.height(),
                t.elapsed().as_secs_f64() * 1000.0
            ),
            Ok(Ok(Err(e))) => eprintln!("[SELFTEST] capture daemon: error: {e}"),
            Ok(Err(e)) => eprintln!("[SELFTEST] capture daemon: join error: {e}"),
            Err(_) => eprintln!(
                "[SELFTEST] capture daemon: TIMED OUT after 60s — even the recovery \
                 ladder is stuck"
            ),
        }
        return Ok(());
    }

    // ── DEBUG CLI subcommands (no MCP) ──────────────────────────────
    if let Some(app) = cli.dump_ax.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, _frame)) => {
                eprintln!("[dump-ax] {app:?} -> pid {pid}");
                print!("{}", nova::platform::ui_tree().dump_tree(pid, 4000));
            }
            None => eprintln!("[dump-ax] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }
    if let Some(app) = cli.marks.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, frame)) => {
                eprintln!("[marks] {app:?} -> pid {pid} clip={frame:?}");
                let els = nova::platform::ui_tree().collect_actionable(pid, 400, Some(frame));
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
                    nova::platform::mac::elements::debug::hit_dump(pid, frame, 24.0, 280.0)
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
                print!(
                    "{}",
                    nova::platform::mac::elements::debug::ax_warm_probe(pid, frame, 12)
                );
            }
            None => eprintln!("[ax-warm] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }

    // Request Screen Recording access from THIS (server) process before serving —
    // it surfaces the first-run system prompt and is a no-op once granted. Done
    // here, not in the headless capture worker (which can't show a prompt).
    let screen_ok = nova::platform::mac::geometry::request_screen_recording_access();
    tracing::info!(
        "Screen Recording access: {}",
        if screen_ok {
            "granted"
        } else {
            "not granted — accept the prompt, or add the nova binary in System \
             Settings → Privacy & Security → Screen Recording"
        }
    );
    // Log the TCC attribution picture once at startup. When nova is a child of
    // another app, `responsible/parent=` shows whose Screen Recording grant the OS
    // actually checks — if that parent is ad-hoc-signed, its grant won't persist
    // across rebuilds and `preflight=false` here even though nova is signed.
    tracing::info!(
        "permission diagnostics: {}",
        nova::platform::mac::geometry::permission_diagnostics()
    );

    if cli.http {
        nova::server::run_http(&cli.addr).await
    } else {
        nova::server::run_stdio().await
    }
}
