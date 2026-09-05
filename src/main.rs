use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Nova — Computer Use MCP Server
///
/// A macOS + Windows desktop control MCP server that gives LLMs the ability to
/// capture screenshots, control mouse/keyboard, manage windows, and introspect apps.
#[derive(Parser, Debug)]
#[command(name = "nova", version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Run in Streamable HTTP mode (default: stdio)
    #[arg(long)]
    http: bool,

    /// Connect stdio to the independently launched Nova.app service. The CLI
    /// is only a byte proxy; desktop APIs and macOS permission checks stay in
    /// the app process.
    #[arg(long, conflicts_with = "http")]
    connect: bool,

    /// INTERNAL: run the app-owned MCP listener. LaunchServices selects this
    /// automatically when it starts the executable inside Nova.app with no
    /// arguments; the flag exists for transport tests and diagnostics.
    #[arg(long, hide = true, conflicts_with_all = ["http", "connect"])]
    app_service: bool,

    /// HTTP listen address (default: 127.0.0.1:3100)
    #[arg(long, default_value = "127.0.0.1:3100")]
    addr: String,

    /// Self-test: do a direct capture (no MCP server) and print timing, then exit.
    /// macOS only — see `run_selftest`'s Windows arm.
    #[arg(long)]
    selftest: bool,

    /// INTERNAL: the SCK-touching half of --selftest, run in a SACRIFICIAL
    /// subprocess. Any process that touches ScreenCaptureKit becomes a replayd
    /// client that collides with the daemon (same-binary identity), so the
    /// direct-stream probe must die before the daemon probe runs. macOS only.
    #[arg(long, hide = true)]
    selftest_direct: bool,

    /// INTERNAL: run as the shared per-user capture daemon. Owns the ONE
    /// ScreenCaptureKit client all nova processes route captures through (two
    /// same-binary processes holding replayd streams evict each other's XPC
    /// identity and wedge — see platform::mac::capture::broker). Spawned on demand; elected
    /// via a flock. Not for direct use. macOS only — Windows' PrintWindow/
    /// BitBlt capture is synchronous and needs no daemon.
    #[arg(long, hide = true)]
    capture_daemon: bool,

    /// INTERNAL (legacy): old pipe-protocol capture worker, kept as a thin
    /// proxy that forwards to the capture daemon — still-running nova servers
    /// from pre-daemon builds spawn this from the binary on disk. Not for
    /// direct use. macOS only.
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

    /// DEBUG: print the `read_ui` text listing nova would return for the app
    /// matching this substring (the AX-first, no-screenshot view), then exit.
    /// No MCP needed.
    #[arg(long, value_name = "APP")]
    read_ui: Option<String>,

    /// DEBUG: hit-test a grid over the content area of the app matching this
    /// substring and print, per distinct element, its role/actions and whether
    /// the actionable-ancestor climb accepts it. Shows WHY visible rows aren't
    /// marked. No MCP needed. macOS only (Accessibility-specific diagnostic).
    #[arg(long, value_name = "APP")]
    hit_dump: Option<String>,

    /// DEBUG: in ONE process, enable web-AX then probe the app repeatedly over
    /// several rounds — tests whether a long-lived process stabilizes Chromium's
    /// full semantic tree (the "Homerow way"). No MCP needed. macOS only.
    #[arg(long, value_name = "APP")]
    ax_warm: Option<String>,

    /// DEBUG: UI Automation smoke test (Windows only) — list the actionable
    /// elements nova would mark for the app matching this substring, then
    /// Invoke one of them (see `--uia-probe-query`) and report whether the
    /// click actually landed. No MCP needed. The P2 UI Automation analog of
    /// `--marks` (discovery) plus a real `click_mark` (activation) in one
    /// shot — proves `collect_actionable`/`WinElementHandle::click` work
    /// against a live app, not just that they compile.
    #[arg(long, value_name = "APP")]
    uia_probe: Option<String>,

    /// With `--uia-probe`: only consider elements whose role/label contains
    /// this substring (case-insensitive) as the Invoke target; defaults to
    /// the first actionable element found.
    #[arg(long, value_name = "SUBSTR")]
    uia_probe_query: Option<String>,

    /// DEBUG: Windows-only WGC smoke test (P4). Resolves the window/app
    /// matching this substring, then captures it via BOTH the raw
    /// `PrintWindow`-only path (the pre-P4 behavior — expected mean≈0/
    /// variance≈0, i.e. black, on a GPU-composited window) and the new
    /// `Windows.Graphics.Capture` path (expected high-variance, non-black),
    /// printing per-channel pixel mean/variance for each and saving a JPEG of
    /// each to the temp dir. Proves the black-bitmap fix against a REAL live
    /// window, not just that it compiles. No MCP needed.
    #[arg(long, value_name = "APP")]
    capture_probe: Option<String>,

    /// DEBUG: list every on-screen window (title, owning app, frame,
    /// visibility) nova's `WindowManager`/`list_windows` sees, then exit. No
    /// MCP needed. Useful to sanity-check window enumeration/attribution
    /// directly (e.g. a UWP app's `ApplicationFrameHost` pid quirk) without
    /// guessing from a screenshot.
    #[arg(long)]
    list_windows: bool,

    /// DEBUG: list the BCP-47 language tags this machine has an installed OCR
    /// pack for (`Windows.Media.Ocr.OcrEngine.AvailableRecognizerLanguages`),
    /// then exit. No MCP needed. Windows only — run this FIRST when `ocr`
    /// comes back empty/erroring, to tell "no pack installed" apart from a
    /// real bug. macOS's Apple Vision OCR ships fully self-contained (no
    /// separate language-pack install step), so there is nothing to list
    /// there.
    #[arg(long)]
    ocr_langs: bool,

    /// DEBUG: OCR smoke test — capture the window whose title/owning-app
    /// matches this substring via the SAME capture path the `ocr` MCP tool
    /// uses, run the platform OCR engine against it, and print each
    /// recognized line's text + clickable center. Proves real end-to-end
    /// recognition (decode + language-pack selection + result mapping), not
    /// just that the platform OCR code compiles/links. Windows only — macOS
    /// already has live coverage via the `ocr` MCP tool itself.
    #[arg(long, value_name = "APP")]
    ocr_probe: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the managed MCP transport. On macOS, connect to the independent
    /// Nova.app (install it separately); on Windows/Linux, serve over stdio.
    Mcp,

    /// Run the pinned official Chrome DevTools MCP server over transparent
    /// stdio, with Nova's privacy-oriented defaults.
    ///
    /// Requires npm and Node.js ^20.19.0, ^22.12.0, or >=23. Use current stable
    /// Chrome; URL allow patterns require Chrome 149+, and WebMCP requires
    /// Chrome 150+.
    ChromeDevtools(nova::chrome_devtools::ChromeDevtoolsArgs),
}

fn main() -> Result<()> {
    // Init logging — MUST go to stderr. In stdio transport, stdout is the
    // JSON-RPC channel; any log line written there corrupts the protocol stream
    // and makes clients (e.g. when RUST_LOG is set) hang waiting for a response.
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    // Some LaunchServices versions append a legacy `-psn_...` process serial
    // argument. It is not a Nova option; ignore only that exact platform
    // marker while retaining clap's strict handling for every real CLI arg.
    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let launch_services_args_only = raw_args
        .iter()
        .skip(1)
        .all(|argument| argument.to_string_lossy().starts_with("-psn_"));
    let cli = Cli::parse_from(
        raw_args
            .iter()
            .filter(|argument| !argument.to_string_lossy().starts_with("-psn_"))
            .cloned(),
    );

    // The sidecar code path does not call Nova's desktop APIs: dispatch it
    // before CoreGraphics, UI Automation, or permission diagnostics. This
    // keeps those APIs out of the launcher path, but macOS ultimately decides
    // responsible-process and TCC attribution for the processes involved.
    if let Some(Commands::ChromeDevtools(options)) = cli.command.as_ref() {
        return nova::chrome_devtools::run(options);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create Nova async runtime")?;

    // Managed clients share one cross-platform command. On macOS this must
    // return before ANY desktop bootstrap or permission diagnostics: changing
    // the MCP host must not move desktop work back into its TCC chain.
    if cfg!(target_os = "macos") && matches!(cli.command, Some(Commands::Mcp)) {
        return run_connector(runtime).context(
            "Nova MCP requires the independent Nova.app service. Install Nova.app in \
             /Applications or ~/Applications and open it once, then reconnect only the \
             Nova MCP server; Bodhi can remain open. Grant desktop permissions to Nova.app",
        );
    }

    // The connector must remain a pure transport proxy. In particular, do
    // this before CoreGraphics initialization so TCC/Desktop responsibility
    // belongs to Nova.app, never Bamboo/Claude/the terminal that spawned the
    // connector.
    if cli.connect {
        tracing::info!("Transport: Nova.app private Unix socket");
        return run_connector(runtime);
    }

    // LaunchServices invokes an application bundle's main executable without
    // arguments. Preserve the historical no-argument stdio behavior for an
    // unbundled CLI while making a real Nova.app independently resident.
    let app_service = cli.app_service
        || (launch_services_args_only && nova::app_service::is_bundled_executable());

    // Enforce the stable application identity before even bootstrapping
    // CoreGraphics. A production macOS binary may not host this endpoint from
    // an unbundled terminal/Bodhi responsibility chain.
    if app_service {
        nova::app_service::ensure_service_identity()?;
    }

    // Per-OS one-time process bootstrap that must run before any capture/
    // window/input call: macOS needs the CoreGraphics window-server connection
    // forced up front (see the macOS arm below); Windows needs Per-Monitor-
    // DPI-v2 declared before any coordinate query (see
    // `platform::windows::init_dpi_awareness`'s doc for why).
    platform_startup_bootstrap();

    // Shared capture daemon: serve capture requests over the per-user socket
    // (macOS only — see `maybe_run_capture_daemon`'s doc).
    maybe_run_capture_daemon(&cli);

    tracing::info!(
        "Nova Computer Use MCP Server v{}",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!(
        "Transport: {}",
        if app_service {
            "Nova.app private Unix socket"
        } else if cli.http {
            "Streamable HTTP"
        } else {
            "stdio"
        }
    );

    if app_service {
        // Permission diagnostics execute in this stable app identity. The
        // app-service listener then creates an independent MCP session for
        // every authenticated same-UID connector.
        log_platform_permissions();
        return run_desktop_service(&runtime, nova::app_service::run());
    }

    if cli.selftest_direct {
        runtime.block_on(run_selftest_direct());
    }

    if cli.selftest {
        return runtime.block_on(run_selftest());
    }

    // ── DEBUG CLI subcommands (no MCP) ──────────────────────────────
    //
    // `dump_ax`/`marks` below go entirely through the neutral
    // `crate::platform::ui_tree()`/`tools::window` facade, so they need no
    // per-OS gating — real Accessibility/UI Automation discovery on both
    // OSes now (see `platform::mac::elements`/`platform::windows::elements`).
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
    if let Some(app) = cli.read_ui.as_deref() {
        match nova::tools::window::pid_for_window(app) {
            Some((pid, frame)) => {
                eprintln!("[read_ui] {app:?} -> pid {pid} clip={frame:?}");
                let els = nova::platform::ui_tree().collect_actionable(pid, 200, Some(frame));
                let (_, lines) = nova::server::build_ui_entries(els, pid);
                println!(
                    "{}",
                    nova::server::format_ui_listing(
                        &lines,
                        &format!("window matching {app:?}"),
                        None
                    )
                );
            }
            None => eprintln!("[read_ui] no on-screen window matching {app:?}"),
        }
        return Ok(());
    }
    if let Some(app) = cli.hit_dump.as_deref() {
        return run_hit_dump(app);
    }
    if let Some(app) = cli.ax_warm.as_deref() {
        return run_ax_warm(app);
    }
    if let Some(app) = cli.uia_probe.as_deref() {
        return run_uia_probe(app, cli.uia_probe_query.as_deref());
    }
    if let Some(app) = cli.capture_probe.as_deref() {
        return run_capture_probe(app);
    }
    if cli.list_windows {
        match nova::tools::window::list_windows() {
            Ok(windows) => {
                eprintln!("[list-windows] {} on-screen windows:", windows.len());
                for w in windows {
                    println!(
                        "{:?} app={:?} @({:.0},{:.0} {:.0}x{:.0}) visible={}",
                        w.title, w.app_name, w.x, w.y, w.width, w.height, w.is_visible
                    );
                }
            }
            Err(e) => eprintln!("[list-windows] failed: {e}"),
        }
        return Ok(());
    }
    if cli.ocr_langs {
        return run_ocr_langs();
    }
    if let Some(app) = cli.ocr_probe.as_deref() {
        return run_ocr_probe(app);
    }

    // Per-OS permission/capability diagnostics, logged once before serving.
    log_platform_permissions();

    run_desktop_service(&runtime, async move {
        if cli.http {
            nova::server::run_http(&cli.addr).await
        } else {
            nova::server::run_stdio().await
        }
    })
}

/// Only for the pure connector's final return from main. Forwarding has
/// finished (including successful stdout flush) before this consumes its
/// dedicated runtime. Tokio's blocking stdin read cannot be cancelled while
/// the host keeps the pipe open; waiting for it here would hide service EOF.
/// The process exits immediately after returning this result and the OS
/// reclaims that remaining read/thread. Never reuse this for a resident server
/// or an embedded runtime: background shutdown alone does not cancel stdin.
fn run_connector(runtime: tokio::runtime::Runtime) -> Result<()> {
    let result = runtime.block_on(nova::app_service::connect_stdio());
    runtime.shutdown_background();
    result
}

/// Desktop servers remain live while macOS delivers application inventory
/// changes on its main run loop. Pure connectors never enter this path.
fn run_desktop_service(
    runtime: &tokio::runtime::Runtime,
    service: impl std::future::Future<Output = Result<()>> + Send + 'static,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        nova::platform::mac::event_loop::run(runtime, service)
    }
    #[cfg(not(target_os = "macos"))]
    {
        runtime.block_on(service)
    }
}

// ── Per-OS startup bootstrap ─────────────────────────────────────────

/// Bootstrap the CoreGraphics / window-server connection BEFORE any
/// ScreenCaptureKit call. Without this, capture from this subprocess either
/// SIGABRTs (CGS_REQUIRE_INIT) or hangs in replayd-connection churn. See
/// `nova::platform::mac::capture::init_core_graphics`.
#[cfg(target_os = "macos")]
fn platform_startup_bootstrap() {
    nova::platform::mac::capture::init_core_graphics();
}

/// Declare Per-Monitor-DPI-v2 awareness before any `GetWindowRect`/
/// `GetSystemMetrics`/`SendInput` call — see
/// `nova::platform::windows::init_dpi_awareness`'s doc for why coordinate
/// correctness depends on this running first.
#[cfg(target_os = "windows")]
fn platform_startup_bootstrap() {
    nova::platform::windows::init_dpi_awareness();
}

/// Serve the shared macOS ScreenCaptureKit capture daemon / legacy pipe-proxy
/// if requested via the hidden `--capture-daemon`/`--capture-worker` flags
/// (both diverge — see `platform::mac::capture::broker::{run_daemon,
/// run_worker_proxy}` — so this never returns if either fires). These
/// low-level paths are diagnostics/plumbing for the capture daemon ITSELF,
/// not tool-layer logic — they call `platform::mac::capture` directly,
/// bypassing the `ScreenCapture` trait, same rationale as the debug CLI's
/// direct `platform::mac::elements::debug` calls in `run_hit_dump`/
/// `run_ax_warm` below.
#[cfg(target_os = "macos")]
fn maybe_run_capture_daemon(cli: &Cli) {
    if cli.capture_daemon {
        nova::platform::mac::capture::broker::run_daemon();
    }
    // Legacy worker entry point: proxy the old pipe protocol into the daemon.
    if cli.capture_worker {
        nova::platform::mac::capture::broker::run_worker_proxy();
    }
}

/// Windows' PrintWindow/BitBlt capture is synchronous and needs no daemon
/// process at all (see `platform::windows::capture`'s module doc), so these
/// flags have no Windows analog — reported cleanly rather than silently
/// ignored, in case a stale launch script carries them over from a macOS
/// deployment.
#[cfg(target_os = "windows")]
fn maybe_run_capture_daemon(cli: &Cli) {
    if cli.capture_daemon || cli.capture_worker {
        eprintln!(
            "--capture-daemon/--capture-worker are macOS-only plumbing for the shared \
             ScreenCaptureKit daemon (see platform::mac::capture::broker) — Windows' \
             PrintWindow/BitBlt capture is synchronous and needs no daemon, so these flags \
             are no-ops here."
        );
    }
}

// ── --selftest / --selftest-direct ───────────────────────────────────

/// SCK-touching probes, isolated in this short-lived process. Our exit closes
/// the replayd XPC connection these open — leaving it open in the main
/// selftest process would wedge the daemon probe that follows. Always exits
/// the process (diverges); see the call site in `main`.
#[cfg(target_os = "macos")]
async fn run_selftest_direct() -> ! {
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

/// Windows has no ScreenCaptureKit/replayd-style wedge to isolate — GDI/
/// PrintWindow capture is synchronous in-process — so there is nothing for a
/// sacrificial subprocess to probe. Not reachable directly (the hidden
/// `--selftest-direct` flag has no public entry point on Windows either), but
/// kept as a clean divergent stub so the crate links if it ever is.
#[cfg(target_os = "windows")]
async fn run_selftest_direct() -> ! {
    eprintln!(
        "--selftest-direct is macOS-only (ScreenCaptureKit capture-daemon diagnostics); \
         not applicable on Windows."
    );
    std::process::exit(0);
}

/// Direct-path + capture-daemon probes, with timing. See the call site in
/// `main` — returns `Ok(())` on the daemon-path timeout/error paths too
/// (`--selftest`'s job is to REPORT a hang, not to fail the process).
#[cfg(target_os = "macos")]
async fn run_selftest() -> Result<()> {
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
                        Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
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
    Ok(())
}

/// Windows has no capture daemon to probe (see `run_selftest_direct`'s
/// Windows arm) — point at the equivalent, always-available sanity check
/// instead of pretending to run a diagnostic that doesn't apply.
#[cfg(target_os = "windows")]
async fn run_selftest() -> Result<()> {
    eprintln!(
        "--selftest is macOS-only (ScreenCaptureKit + capture-daemon diagnostics); not \
         applicable on Windows, which has no capture daemon to probe (GDI/PrintWindow capture \
         is synchronous and in-process — see platform::windows::capture). Use the `screenshot` \
         or `list_windows` MCP tools directly to sanity-check capture on this OS."
    );
    Ok(())
}

// ── --hit-dump / --ax-warm (macOS Accessibility diagnostics) ─────────

#[cfg(target_os = "macos")]
fn run_hit_dump(app: &str) -> Result<()> {
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
    Ok(())
}

/// `--hit-dump` is an Accessibility (AX) hit-testing diagnostic with no
/// Windows analog yet — the equivalent UI Automation-based tree walk is
/// tracked as later-phase work alongside `platform::windows::elements`'s
/// `UiTree` stub.
#[cfg(target_os = "windows")]
fn run_hit_dump(_app: &str) -> Result<()> {
    eprintln!(
        "--hit-dump is macOS-only (Accessibility hit-testing diagnostics); UI Automation-based \
         diagnostics are tracked for a later phase on Windows."
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_ax_warm(app: &str) -> Result<()> {
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
    Ok(())
}

/// `--ax-warm` probes macOS's Accessibility "keep the Chromium tree warm"
/// behavior specifically — no Windows analog (UI Automation trees don't have
/// the same cold-tree-reaping quirk); tracked alongside `run_hit_dump`.
#[cfg(target_os = "windows")]
fn run_ax_warm(_app: &str) -> Result<()> {
    eprintln!(
        "--ax-warm is macOS-only (probes Accessibility's Chromium warm-tree behavior); no \
         Windows analog (tracked for a later phase alongside UI Automation support)."
    );
    Ok(())
}

// ── --uia-probe (Windows UI Automation P2 smoke test) ────────────────

/// Discover UI Automation actionable elements for the app matching `app`
/// (same discovery path `screenshot(marks=true)` uses), print them, then
/// `click()` one of them (the first matching `query`, or the first overall)
/// and report whether the Invoke/Toggle/SelectionItem/ExpandCollapse actually
/// landed. Exists to prove `WinUiTree::collect_actionable`/`WinElementHandle::click`
/// work against a REAL live app — not just that they compile — without a full
/// MCP round trip.
#[cfg(target_os = "windows")]
fn run_uia_probe(app: &str, query: Option<&str>) -> Result<()> {
    let Some((pid, frame)) = nova::tools::window::pid_for_window(app) else {
        eprintln!("[uia-probe] no on-screen window matching {app:?}");
        return Ok(());
    };
    eprintln!("[uia-probe] {app:?} -> pid {pid} clip={frame:?}");
    let elements = nova::platform::ui_tree().collect_actionable(pid, 400, Some(frame));
    eprintln!("[uia-probe] {} actionable elements:", elements.len());
    for (i, (el, _)) in elements.iter().enumerate() {
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

    let target = query
        .and_then(|q| {
            let q_lower = q.to_lowercase();
            elements.iter().find(|(el, _)| {
                format!("{} {}", el.role, el.label)
                    .to_lowercase()
                    .contains(&q_lower)
            })
        })
        .or_else(|| elements.first());

    match target {
        Some((el, handle)) => {
            eprintln!("[uia-probe] clicking {} {:?} ...", el.role, el.label);
            match handle.click() {
                Ok(action) => eprintln!(
                    "[uia-probe] SUCCESS: performed {action} on {} {:?}",
                    el.role, el.label
                ),
                Err(e) => eprintln!("[uia-probe] CLICK FAILED: {e}"),
            }
        }
        None => eprintln!(
            "[uia-probe] no actionable element to click (list was empty, or --uia-probe-query \
             matched nothing)"
        ),
    }
    Ok(())
}

/// `--uia-probe` is Windows-only — macOS already has real end-to-end evidence
/// via `--marks` (discovery) plus the live `click_mark`/`screenshot` MCP tools
/// exercised in `tests/e2e_safari_google.rs`; this diagnostic exists
/// specifically to smoke-test the NEW Windows UI Automation path.
#[cfg(target_os = "macos")]
fn run_uia_probe(_app: &str, _query: Option<&str>) -> Result<()> {
    eprintln!(
        "--uia-probe is Windows-only (a P2 UI Automation discovery+Invoke smoke test); macOS \
         already proves marks/click_mark via --marks and the live MCP tools."
    );
    Ok(())
}

// ── --ocr-langs / --ocr-probe (Windows Windows.Media.Ocr P3 smoke test) ──

/// List the BCP-47 language tags this machine has an OCR pack installed for.
/// Run this BEFORE assuming an `ocr`/`--ocr-probe` failure is a code bug: an
/// empty list means the VM/machine has no `Windows.Media.Ocr` language pack
/// at all, which no amount of `platform::windows::ocr` code can work around.
#[cfg(target_os = "windows")]
fn run_ocr_langs() -> Result<()> {
    match nova::platform::windows::ocr::available_languages() {
        Ok(tags) if tags.is_empty() => eprintln!(
            "[ocr-langs] AvailableRecognizerLanguages() returned 0 languages — no Windows OCR \
             language pack is installed on this machine (install one via Settings > Time & \
             Language > Language & region > Add a language > Options > Add \"Optical character \
             recognition\")"
        ),
        Ok(tags) => {
            eprintln!("[ocr-langs] {} OCR language pack(s) available:", tags.len());
            for tag in tags {
                println!("{tag}");
            }
        }
        Err(e) => eprintln!("[ocr-langs] AvailableRecognizerLanguages() failed: {e}"),
    }
    Ok(())
}

/// `--ocr-langs` is Windows-only — Apple Vision OCR ships fully self-contained
/// on macOS, with no separate per-language pack to install or list.
#[cfg(target_os = "macos")]
fn run_ocr_langs() -> Result<()> {
    eprintln!(
        "--ocr-langs is Windows-only (lists installed Windows.Media.Ocr language packs); \
         macOS's Apple Vision OCR needs no separate language-pack install."
    );
    Ok(())
}

/// Capture the window matching `app` via the SAME path the `ocr` MCP tool
/// uses (`ScreenCapture::capture_window` → `finish_capture` → JPEG bytes),
/// then run `platform::ocr().recognize` against it and print every recognized
/// line's text and clickable center. This is the mandatory end-to-end smoke
/// for P3: link success alone doesn't prove `Windows.Media.Ocr` actually
/// recognizes text, only that the code compiles.
#[cfg(target_os = "windows")]
fn run_ocr_probe(app: &str) -> Result<()> {
    use base64::Engine;

    let raw = match nova::platform::screen_capture().capture_window(app) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("[ocr-probe] capture_window({app:?}) failed: {e}");
            return Ok(());
        }
    };
    let capture = match nova::capture::screenshot::finish_capture(
        raw,
        nova::capture::screenshot::CaptureOptions::default(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ocr-probe] finish_capture failed: {e}");
            return Ok(());
        }
    };
    let jpeg = match base64::engine::general_purpose::STANDARD.decode(&capture.result.base64_image)
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[ocr-probe] base64 decode of the captured JPEG failed: {e}");
            return Ok(());
        }
    };
    let (w, h) = (capture.result.width, capture.result.height);
    eprintln!("[ocr-probe] {app:?} captured {w}x{h} px; running OCR ...");
    // Same default language priority the `ocr` MCP tool uses when the caller
    // doesn't override `languages`.
    let languages = ["zh-Hans", "en-US"];
    match nova::platform::ocr().recognize(&jpeg, w, h, &languages) {
        Ok(lines) => {
            eprintln!("[ocr-probe] {} line(s) recognized:", lines.len());
            for (i, line) in lines.iter().enumerate() {
                println!(
                    "[{}] {:?} conf={:.2} center=({:.0},{:.0})",
                    i + 1,
                    line.text,
                    line.confidence,
                    line.center.0,
                    line.center.1
                );
            }
        }
        Err(e) => eprintln!("[ocr-probe] recognize() failed: {e}"),
    }
    Ok(())
}

/// `--ocr-probe` is Windows-only — macOS's Apple Vision OCR path is already
/// exercised live by the `ocr` MCP tool itself; this diagnostic exists
/// specifically to smoke-test the NEW `Windows.Media.Ocr` path end-to-end
/// (capture → decode → recognize → mapped centers) without a full MCP round
/// trip.
#[cfg(target_os = "macos")]
fn run_ocr_probe(_app: &str) -> Result<()> {
    eprintln!(
        "--ocr-probe is Windows-only (an end-to-end Windows.Media.Ocr capture+recognize smoke \
         test); macOS's Apple Vision OCR path is already exercised live by the `ocr` MCP tool."
    );
    Ok(())
}

// ── --capture-probe (Windows WGC P4 smoke test) ───────────────────────

/// Run BOTH window-capture paths (raw `PrintWindow`-only, then WGC) against
/// the same live window matching `app` and print pixel-statistics evidence
/// for each — see `platform::windows::capture::capture_probe`'s doc for what
/// "evidence" means here (mean/variance contrast) and why the two paths are
/// run independently rather than short-circuiting on the first success.
#[cfg(target_os = "windows")]
fn run_capture_probe(app: &str) -> Result<()> {
    match nova::platform::windows::capture::capture_probe(app) {
        Ok(report) => print!("{report}"),
        Err(e) => eprintln!("[capture-probe] {app:?} -> failed: {e}"),
    }
    Ok(())
}

/// `--capture-probe` is Windows-only — it exists specifically to smoke-test
/// the new Windows.Graphics.Capture path (P4) against the `PrintWindow`
/// black-bitmap bug, which has no macOS analog (ScreenCaptureKit never had
/// this failure mode).
#[cfg(target_os = "macos")]
fn run_capture_probe(_app: &str) -> Result<()> {
    eprintln!(
        "--capture-probe is Windows-only (a P4 Windows.Graphics.Capture vs. PrintWindow \
         pixel-stats smoke test); macOS's ScreenCaptureKit capture has no black-bitmap bug to \
         demonstrate a fix for."
    );
    Ok(())
}

// ── Per-OS permission/capability diagnostics ──────────────────────────

/// Request Screen Recording access from THIS (server) process before serving —
/// it surfaces the first-run system prompt and is a no-op once granted. Done
/// here, not in the headless capture worker (which can't show a prompt). Also
/// logs the TCC attribution picture once at startup: when nova is a child of
/// another app, `responsible/parent=` shows whose Screen Recording grant the
/// OS actually checks — if that parent is ad-hoc-signed, its grant won't
/// persist across rebuilds and `preflight=false` here even though nova is
/// signed.
#[cfg(target_os = "macos")]
fn log_platform_permissions() {
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
    tracing::info!(
        "permission diagnostics: {}",
        nova::platform::mac::geometry::permission_diagnostics()
    );
}

/// Windows' GDI `BitBlt`/`PrintWindow` capture needs no OS-level screen-
/// recording grant (unlike macOS's TCC) — logged once so this asymmetry is
/// obvious from the startup log rather than a silent difference.
#[cfg(target_os = "windows")]
fn log_platform_permissions() {
    tracing::info!(
        "Windows: PrintWindow/BitBlt capture needs no OS-level screen-recording permission \
         (unlike macOS's Screen Recording TCC grant). If a specific app's window capture comes \
         back blank, that app itself may not support PrintWindow's PW_RENDERFULLCONTENT (rare) \
         rather than a permission issue — see platform::windows::capture."
    );
}

// ── Headless variants (every other OS) ────────────────────────────────
//
// The per-OS functions above all get a third arm on OSes with no desktop
// backend (see `platform::headless`'s module doc): the server still starts
// and serves MCP introspection, the desktop-diagnostic CLI flags report
// cleanly instead of vanishing from the binary, and there is no bootstrap/
// daemon/permission machinery to run.

/// What every desktop-diagnostic CLI flag prints on a headless build.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const HEADLESS_DIAG: &str = "this is a headless nova build (no macOS/Windows desktop backend): \
     the MCP server starts and lists its tools, but desktop diagnostics and desktop control are \
     unavailable on this OS.";

/// Nothing to bootstrap: no window-server connection (macOS) and no DPI
/// awareness to declare (Windows).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_startup_bootstrap() {}

/// No capture daemon exists on a headless build (the macOS-only plumbing these
/// flags drive) — reported cleanly rather than silently ignored, mirroring the
/// Windows arm.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn maybe_run_capture_daemon(cli: &Cli) {
    if cli.capture_daemon || cli.capture_worker {
        eprintln!("--capture-daemon/--capture-worker are macOS-only plumbing; {HEADLESS_DIAG}");
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn run_selftest_direct() -> ! {
    eprintln!("--selftest-direct: {HEADLESS_DIAG}");
    std::process::exit(0);
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn run_selftest() -> Result<()> {
    eprintln!("--selftest: {HEADLESS_DIAG}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_hit_dump(_app: &str) -> Result<()> {
    eprintln!("--hit-dump: {HEADLESS_DIAG}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_ax_warm(_app: &str) -> Result<()> {
    eprintln!("--ax-warm: {HEADLESS_DIAG}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_uia_probe(_app: &str, _query: Option<&str>) -> Result<()> {
    eprintln!("--uia-probe: {HEADLESS_DIAG}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_ocr_langs() -> Result<()> {
    eprintln!("--ocr-langs: {HEADLESS_DIAG}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_ocr_probe(_app: &str) -> Result<()> {
    eprintln!("--ocr-probe: {HEADLESS_DIAG}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run_capture_probe(_app: &str) -> Result<()> {
    eprintln!("--capture-probe: {HEADLESS_DIAG}");
    Ok(())
}

/// No OS permission concept applies — log the headless story once so a
/// registry probe reading the startup log sees why tools will error.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn log_platform_permissions() {
    tracing::info!("{}", HEADLESS_DIAG);
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn managed_mcp_is_an_explicit_subcommand() {
        let cli = Cli::try_parse_from(["nova", "mcp"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Mcp)));
        assert!(!cli.http && !cli.connect && !cli.app_service);
    }

    #[test]
    fn managed_mcp_rejects_other_transport_and_desktop_modes() {
        for arguments in [
            vec!["nova", "--http", "mcp"],
            vec!["nova", "--connect", "mcp"],
            vec!["nova", "--app-service", "mcp"],
            vec!["nova", "--capture-daemon", "mcp"],
            vec!["nova", "--selftest", "mcp"],
            vec!["nova", "mcp", "--http"],
            vec!["nova", "mcp", "--connect"],
            vec!["nova", "mcp", "--capture-daemon"],
            vec!["nova", "mcp", "chrome-devtools"],
        ] {
            assert!(Cli::try_parse_from(&arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn existing_cli_modes_keep_their_meaning() {
        assert!(Cli::try_parse_from(["nova"]).unwrap().command.is_none());
        assert!(Cli::try_parse_from(["nova", "--connect"]).unwrap().connect);
        let http = Cli::try_parse_from(["nova", "--http", "--addr", "127.0.0.1:3210"]).unwrap();
        assert!(http.http);
        assert_eq!(http.addr, "127.0.0.1:3210");
        assert!(matches!(
            Cli::try_parse_from(["nova", "chrome-devtools"])
                .unwrap()
                .command,
            Some(Commands::ChromeDevtools(_))
        ));
    }
}
