//! Shared capture broker — ONE ScreenCaptureKit client per user, by construction.
//!
//! Root cause this exists for (diagnosed 2026-06-13 from replayd's own logs):
//! replayd's `RPConnectionManager` identifies clients by EXECUTABLE PATH, not by
//! pid. Two live processes of the same binary (two nova instances, or a nova
//! server plus its capture subprocess) holding ScreenCaptureKit window streams
//! evict each other's RPClient identity in an endless connect/cancel storm
//! (observed: >11,000 XPC accepts in 5s). replayd actually CREATES the second
//! stream successfully — but the `startCapture` completion reply is lost in the
//! storm, so the caller blocks forever on a condvar with no timeout. Killing
//! replayd does not help (`killall replayd` is a no-op — replayd ignores
//! SIGTERM; `killall -9` works but surviving clients re-wedge the fresh replayd
//! by reconnecting). The ONLY reliable cure is killing the processes that hold
//! the streams.
//!
//! So: every nova process routes captures through a single per-user daemon
//! (`nova --capture-daemon`, elected via an exclusive flock) that owns the one
//! warm `StreamCapturer`. No two same-binary processes ever hold replayd
//! streams concurrently, which removes the wedge CLASS — and as a bonus all
//! sessions share one warm stream instead of cold-starting their own.
//!
//! Self-healing: the daemon bounds every request with a watchdog. If a capture
//! wedges inside an uncancellable ScreenCaptureKit call, the daemon reports a
//! structured error to the waiting client and EXITS — its death severs every
//! XPC connection, which is exactly the "restart nova" manual remedy, automated
//! and scoped. Clients respawn it on the next capture. The client adds a
//! recovery ladder on top (kill daemon → retry; kill all stray capture
//! processes + `killall -9 replayd` → retry).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::capture::screenshot::{step, RawCapture};
use crate::display::view::ViewFrame;

/// Bumped whenever the wire protocol changes. A client that connects to a
/// daemon with an OLDER proto (or a different binary mtime — i.e. the binary
/// was rebuilt since the daemon started) kills it and spawns a fresh one; a
/// client that meets a NEWER daemon fails itself instead (killing it would
/// just respawn the same newer binary — an unwinnable assassination loop).
const PROTO_VERSION: u32 = 3;

/// How long one request may sit QUEUED behind other requests before the daemon
/// answers "busy" (a clean, non-wedge error — queue delay means the capture
/// thread is making progress, the opposite of a wedge).
const QUEUE_BUDGET: Duration = Duration::from_secs(20);

/// How long one request may EXECUTE inside ScreenCaptureKit before the daemon
/// declares it wedged, tells the client, and exits for a clean slate.
/// Generous: a healthy capture (resolve + stream start + first frame) is well
/// under 4s even cold. Measured from job start, never including queue time —
/// a watchdog that counted queue time would suicide the daemon (and escalate
/// clients to `killall -9 replayd`) under perfectly healthy concurrency.
const DAEMON_WATCHDOG: Duration = Duration::from_secs(8);

/// Client-side read timeout per exchange. Above the daemon's worst HONEST
/// reply latency ([`QUEUE_BUDGET`] + [`DAEMON_WATCHDOG`]) so a busy or wedged
/// daemon always gets to deliver its structured error first; this only trips
/// when the daemon stopped answering without dying (e.g. SIGSTOP).
const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(35);

/// Exit code for "capture wedged — exiting so process death clears it".
const EXIT_WEDGE: i32 = 70;

/// How long a client keeps trying to connect (spawning the daemon once) before
/// giving up.
const CONNECT_BUDGET: Duration = Duration::from_secs(5);

/// Daemon exits after this long with no client connections and no requests —
/// frees the warm stream, and lets a rebuilt binary take over naturally.
/// Overridable via NOVA_DAEMON_IDLE_EXIT_SECS (tests use a short TTL so stray
/// test daemons — same binary path! — can't linger and collide with the real
/// one).
fn daemon_idle_exit() -> Duration {
    std::env::var("NOVA_DAEMON_IDLE_EXIT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(15 * 60))
}

/// What to capture. One JSON line per request on the socket (and, for the
/// legacy `--capture-worker` pipe proxy, on stdin).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CaptureRequest {
    Display,
    Window { query: String },
    Region { rect: (f64, f64, f64, f64) },
    /// Enumerate on-screen windows (metadata only, no pixels). Window
    /// enumeration ALSO goes through the daemon: `SCShareableContent` keeps a
    /// replayd XPC client connection open in whatever process calls it, and a
    /// long-lived same-binary process holding one is a storm participant (the
    /// 23:32 episode's parent had 11k XPC accept/cancel cycles).
    Windows,
}

/// One on-screen window's metadata, as served by the daemon. Listed in the
/// z-order `SCShareableContent` returns (frontmost first), which
/// `frontmost_app_pid` relies on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireWindow {
    pub title: String,
    pub app_name: String,
    pub pid: i32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub is_visible: bool,
}

/// A successful daemon reply.
pub enum Reply {
    Image(RawCapture),
    Windows(Vec<WireWindow>),
}

/// Response header (one JSON line). When `ok` with `len > 0`, exactly `len`
/// raw RGB8 bytes follow on the pipe/socket; a `Windows` reply carries its
/// payload inline in `windows` instead.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    ok: bool,
    error: Option<String>,
    /// True when the error is a wedge (capture hung inside ScreenCaptureKit and
    /// the daemon is exiting) rather than a clean capture failure (e.g. "no
    /// window matching ..."). Tells the client to run its recovery ladder.
    #[serde(default)]
    wedge: bool,
    /// Window-enumeration payload (`CaptureRequest::Windows` replies only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    windows: Option<Vec<WireWindow>>,
    width: u32,
    height: u32,
    origin: (f64, f64),
    region: (f64, f64),
    screenshot: (f64, f64),
    window_pid: Option<i32>,
    len: usize,
}

/// First line the daemon sends on every fresh connection.
#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    proto: u32,
    pid: i32,
    /// mtime (ms) of the daemon's executable AT STARTUP. A client whose binary
    /// on disk is newer treats the daemon as stale and replaces it.
    exe_mtime_ms: u64,
    /// Whether THIS daemon's responsibility chain holds the Screen Recording
    /// TCC grant (CGPreflightScreenCaptureAccess, computed per connection).
    /// The grant is checked against whoever SPAWNED the daemon — a daemon
    /// spawned under a denied host would fail every capture forever, so a
    /// granted client treats a denied daemon as stale and replaces it (the
    /// respawn then inherits the granted chain).
    #[serde(default)]
    preflight: bool,
}

fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("NOVA_CAPTURE_SOCK") {
        return PathBuf::from(p);
    }
    // SAFETY: argless libc call.
    let uid = unsafe { libc::getuid() };
    // One daemon per (uid, executable path): replayd keys client identity on
    // the exe path, so two INSTALLS at different paths are distinct replayd
    // identities and must each own their own daemon — sharing one socket would
    // make their mtime handshakes kill each other's daemon on every capture.
    // Inline FNV-1a over the canonical path: stable across builds and rustc
    // versions (std's DefaultHasher is not guaranteed stable; an unstable hash
    // would split same-path rebuilds onto two sockets, reintroducing the
    // same-binary storm).
    let exe = capture_bin()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in exe.as_os_str().as_encoded_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    PathBuf::from(format!("/tmp/nova-capture-{uid}-{h:016x}.sock"))
}

fn lock_path() -> PathBuf {
    let mut p = socket_path().into_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

/// mtime (ms since epoch) of this process's executable on disk right now.
fn exe_mtime_ms() -> u64 {
    capture_bin()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The binary to run as the daemon — this same executable, overridable for
/// tests (which run under the test harness binary, not nova).
fn capture_bin() -> Option<PathBuf> {
    match std::env::var_os("NOVA_CAPTURE_BIN") {
        Some(p) => Some(PathBuf::from(p)),
        None => std::env::current_exe().ok(),
    }
}

// ── Daemon side ─────────────────────────────────────────────────────

/// Progress of one queued request, so the watchdog can tell "still queued
/// behind other work" (busy, not a wedge) from "stuck inside ScreenCaptureKit"
/// (a wedge).
enum JobEvent {
    /// The capture thread dequeued this job and is about to execute it.
    Started,
    Done(Result<Reply, String>),
}

struct Job {
    req: CaptureRequest,
    reply: std::sync::mpsc::Sender<JobEvent>,
    /// Set by the conn thread if it gave up waiting (queue budget exceeded) —
    /// the capture thread then skips the job instead of doing orphaned work.
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

/// Daemon-side window enumeration (the only process allowed to hold the
/// `SCShareableContent` replayd connection). Unfiltered beyond on-screen +
/// non-desktop; callers apply their own filters.
fn list_windows_wire() -> Result<Vec<WireWindow>, String> {
    step("daemon: list windows (SCShareableContent::get)");
    let content = screencapturekit::shareable_content::SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .map_err(|e| format!("SCShareableContent::get: {e}"))?;
    Ok(content
        .windows()
        .iter()
        .map(|w| {
            let frame = w.frame();
            let (app_name, pid) = w
                .owning_application()
                .map(|a| (a.application_name(), a.process_id()))
                .unwrap_or_default();
            WireWindow {
                title: w.title().unwrap_or_default(),
                app_name,
                pid,
                x: frame.origin.x,
                y: frame.origin.y,
                width: frame.size.width,
                height: frame.size.height,
                is_visible: w.is_on_screen(),
            }
        })
        .collect())
}

/// Run the capture daemon: win the per-user flock election (or exit), bind the
/// socket, and serve capture requests through the single warm StreamCapturer.
/// Never returns.
pub fn run_daemon() -> ! {
    crate::capture::screenshot::enable_step_trace();

    // Election: exactly one daemon per socket path. The flock dies with the
    // process, so a SIGKILLed daemon releases it instantly — no stale-pid logic.
    let lockf = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path())
    {
        Ok(f) => f,
        Err(e) => {
            step(&format!("daemon: cannot open lock file: {e}"));
            std::process::exit(1);
        }
    };
    {
        use std::os::fd::AsRawFd;
        // SAFETY: flock on an fd we own.
        if unsafe { libc::flock(lockf.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            step("daemon: another daemon holds the lock — exiting (lost election)");
            std::process::exit(0);
        }
    }
    // Record our pid in the lock file: a client that can't complete a handshake
    // (half-dead daemon) still needs a target to kill.
    {
        let _ = lockf.set_len(0);
        let mut f = &lockf;
        let _ = write!(f, "{}", std::process::id());
        let _ = f.flush();
    }
    // Lock is ours: the socket file, if present, is from a dead daemon.
    let sock = socket_path();
    let _ = std::fs::remove_file(&sock);
    // Restrict the socket from birth (no bind→chmod window where another
    // local user could connect).
    // SAFETY: process-wide umask set/restore around one bind; this process is
    // single-threaded at this point (capture/conn threads start later).
    let saved_umask = unsafe { libc::umask(0o177) };
    let listener = UnixListener::bind(&sock);
    unsafe { libc::umask(saved_umask) };
    let listener = match listener {
        Ok(l) => l,
        Err(e) => {
            step(&format!("daemon: bind {} failed: {e}", sock.display()));
            std::process::exit(1);
        }
    };
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    step(&format!(
        "daemon: serving on {} (pid={}, proto={PROTO_VERSION})",
        sock.display(),
        std::process::id()
    ));

    let exe_mtime = exe_mtime_ms();

    // The capture thread owns the StreamCapturer (and pumps the run loop the
    // unlock observer + frame delivery are serviced on).
    let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
    std::thread::spawn(move || capture_thread(job_rx));

    let active = Arc::new(AtomicI64::new(0));
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    listener
        .set_nonblocking(true)
        .expect("listener nonblocking");
    loop {
        match listener.accept() {
            Ok((conn, _)) => {
                let _ = conn.set_nonblocking(false);
                active.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut t) = last_activity.lock() {
                    *t = Instant::now();
                }
                let job_tx = job_tx.clone();
                let active = active.clone();
                let last_activity = last_activity.clone();
                std::thread::spawn(move || {
                    serve_conn(conn, exe_mtime, &job_tx, &last_activity);
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
                let idle = last_activity
                    .lock()
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                if active.load(Ordering::SeqCst) == 0 && idle >= daemon_idle_exit() {
                    step("daemon: idle with no clients — exiting");
                    std::process::exit(0);
                }
            }
            Err(e) => {
                step(&format!("daemon: accept error: {e}"));
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// The one thread that talks to ScreenCaptureKit. Owns the warm stream;
/// between requests it pumps the run loop (frame delivery + the unlock
/// distributed notification land there) and does stream housekeeping (eager
/// unlock invalidation, idle TTL).
fn capture_thread(rx: std::sync::mpsc::Receiver<Job>) {
    let mut capturer = crate::capture::stream::StreamCapturer::new();
    loop {
        // Pump doubles as the poll sleep — keeps notification delivery and
        // SCStream scheduling serviced while idle.
        crate::capture::stream::pump_run_loop(0.02);
        match rx.try_recv() {
            Ok(job) => {
                if job.cancelled.load(Ordering::Acquire) {
                    step(&format!("REQ {:?} — skipped (client gave up while queued)", job.req));
                    continue;
                }
                step(&format!("REQ {:?}", job.req));
                let _ = job.reply.send(JobEvent::Started);
                let res = match job.req {
                    CaptureRequest::Display => capturer.capture_display().map(Reply::Image),
                    CaptureRequest::Window { ref query } => {
                        capturer.capture_window(query).map(Reply::Image)
                    }
                    CaptureRequest::Region { rect } => {
                        capturer.capture_region(rect).map(Reply::Image)
                    }
                    CaptureRequest::Windows => list_windows_wire().map(Reply::Windows),
                };
                step(&format!("RESP {}", if res.is_ok() { "ok" } else { "err" }));
                // Receiver gone (its conn thread hit the watchdog and the
                // process is exiting, or the client vanished): nothing to do.
                let _ = job.reply.send(JobEvent::Done(res));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => capturer.housekeeping(),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => std::process::exit(0),
        }
    }
}

fn serve_conn(
    conn: UnixStream,
    exe_mtime_ms: u64,
    job_tx: &std::sync::mpsc::Sender<Job>,
    last_activity: &Arc<Mutex<Instant>>,
) {
    let _ = conn.set_write_timeout(Some(Duration::from_secs(5)));
    let mut writer = match conn.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    // Hello is built per connection: `preflight` can change at runtime (the
    // user granting Screen Recording mid-session must be visible).
    let hello = serde_json::to_string(&Hello {
        proto: PROTO_VERSION,
        pid: std::process::id() as i32,
        exe_mtime_ms,
        preflight: crate::display::geometry::preflight_screen_capture(),
    })
    .unwrap_or_default();
    if writeln!(writer, "{hello}").is_err() {
        return;
    }
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return, // client gone
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(mut t) = last_activity.lock() {
            *t = Instant::now();
        }
        let req: CaptureRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let _ = write_err(&mut writer, &format!("bad request: {e}"), false);
                continue;
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if job_tx
            .send(Job {
                req: req.clone(),
                reply: tx,
                cancelled: cancelled.clone(),
            })
            .is_err()
        {
            // The capture thread itself is gone (panicked) — that's fatal for
            // the whole daemon; exit so clients respawn a healthy one.
            let _ = write_err(&mut writer, "capture thread is gone (panicked?)", true);
            std::process::exit(EXIT_WEDGE);
        }
        // Phase 1 — wait for the job to be DEQUEUED. Expiry here means the
        // daemon is busy with other clients' work (making progress), which is
        // the opposite of a wedge: answer cleanly, don't exit, don't let the
        // client run its kill ladder. A true head-of-line wedge is detected by
        // the EXECUTING request's own conn thread within DAEMON_WATCHDOG; its
        // exit gives every queued waiter an EOF → reconnect-and-retry.
        match rx.recv_timeout(QUEUE_BUDGET) {
            Ok(JobEvent::Started) => {}
            Ok(JobEvent::Done(_)) => unreachable!("Started always precedes Done"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                step(&format!(
                    "daemon: {req:?} still queued after {QUEUE_BUDGET:?} — answering busy"
                ));
                if write_err(
                    &mut writer,
                    &format!(
                        "capture daemon busy (request queued behind other captures for \
                         {QUEUE_BUDGET:?}); try again"
                    ),
                    false,
                )
                .is_err()
                {
                    return;
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = write_err(&mut writer, "capture thread is gone (panicked?)", true);
                std::process::exit(EXIT_WEDGE);
            }
        }
        // Phase 2 — the job is executing; now the clock measures only
        // ScreenCaptureKit itself.
        match rx.recv_timeout(DAEMON_WATCHDOG) {
            Ok(JobEvent::Done(Ok(Reply::Image(raw)))) => {
                if write_ok(&mut writer, &raw).is_err() {
                    return;
                }
            }
            Ok(JobEvent::Done(Ok(Reply::Windows(windows)))) => {
                if write_windows(&mut writer, windows).is_err() {
                    return;
                }
            }
            Ok(JobEvent::Done(Err(e))) => {
                if write_err(&mut writer, &e, false).is_err() {
                    return;
                }
            }
            Ok(JobEvent::Started) => unreachable!("Started is sent exactly once"),
            Err(_) => {
                // The capture thread is stuck inside an uncancellable
                // ScreenCaptureKit call. Exiting severs every XPC connection
                // this process holds — the one cure that provably clears the
                // wedge. Clients respawn us on demand.
                step(&format!(
                    "daemon: {req:?} wedged for {DAEMON_WATCHDOG:?} — exiting for a clean slate"
                ));
                let _ = write_err(
                    &mut writer,
                    &format!(
                        "capture wedged inside ScreenCaptureKit for {DAEMON_WATCHDOG:?}; \
                         capture daemon is restarting itself to clear it"
                    ),
                    true,
                );
                std::process::exit(EXIT_WEDGE);
            }
        }
    }
}

fn write_ok(w: &mut impl Write, raw: &RawCapture) -> std::io::Result<()> {
    let header = Header {
        ok: true,
        error: None,
        wedge: false,
        windows: None,
        width: raw.image.width(),
        height: raw.image.height(),
        origin: raw.view.origin,
        region: raw.view.region,
        screenshot: raw.view.screenshot,
        window_pid: raw.window_pid,
        len: raw.image.as_raw().len(),
    };
    writeln!(w, "{}", serde_json::to_string(&header).unwrap_or_default())?;
    w.write_all(raw.image.as_raw())?;
    w.flush()
}

fn write_windows(w: &mut impl Write, windows: Vec<WireWindow>) -> std::io::Result<()> {
    let header = Header {
        ok: true,
        error: None,
        wedge: false,
        windows: Some(windows),
        width: 0,
        height: 0,
        origin: (0.0, 0.0),
        region: (0.0, 0.0),
        screenshot: (0.0, 0.0),
        window_pid: None,
        len: 0,
    };
    writeln!(w, "{}", serde_json::to_string(&header).unwrap_or_default())?;
    w.flush()
}

fn write_err(w: &mut impl Write, msg: &str, wedge: bool) -> std::io::Result<()> {
    let header = Header {
        ok: false,
        error: Some(msg.to_string()),
        wedge,
        windows: None,
        width: 0,
        height: 0,
        origin: (0.0, 0.0),
        region: (0.0, 0.0),
        screenshot: (0.0, 0.0),
        window_pid: None,
        len: 0,
    };
    writeln!(w, "{}", serde_json::to_string(&header).unwrap_or_default())?;
    w.flush()
}

// ── Legacy pipe proxy (`--capture-worker` compatibility) ───────────

/// Speak the OLD worker pipe protocol on stdin/stdout, forwarding every request
/// to the shared daemon. Nova server processes from pre-daemon builds that are
/// still running spawn `nova --capture-worker` from the binary ON DISK (this
/// build) — this proxy keeps them working AND moves their streams into the
/// daemon, so even legacy parents stop holding their own replayd streams.
pub fn run_worker_proxy() -> ! {
    let client = CaptureClient::new();
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => std::process::exit(0), // parent closed the pipe
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let result = match serde_json::from_str::<CaptureRequest>(trimmed) {
            Ok(req) => client.request(&req),
            Err(e) => Err(format!("bad request: {e}")),
        };
        let mut out = stdout.lock();
        let io = match result {
            Ok(Reply::Image(raw)) => write_ok(&mut out, &raw),
            Ok(Reply::Windows(w)) => write_windows(&mut out, w),
            Err(e) => write_err(&mut out, &e, false),
        };
        if io.is_err() {
            std::process::exit(0);
        }
    }
}

// ── Client side ─────────────────────────────────────────────────────

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    daemon_pid: i32,
}

/// How one capture attempt failed — decides whether the recovery ladder runs.
enum AttemptError {
    /// The daemon answered with a clean capture failure (e.g. "no on-screen
    /// window matching ..."). The daemon is healthy; report it as-is.
    Clean(String),
    /// The daemon reported a wedge, stopped answering, or the connection died.
    Wedge(String),
    /// The daemon cannot exist at all in this environment (its binary exits
    /// immediately — e.g. a test harness without NOVA_CAPTURE_BIN). Escalating
    /// the ladder cannot help and would only kill innocent processes/replayd.
    Fatal(String),
}

/// Client handle to the shared capture daemon. One per nova process (shared by
/// all MCP sessions in it); captures are serialized on the connection.
#[derive(Default)]
pub struct CaptureClient {
    io: Mutex<Option<Conn>>,
}

impl std::fmt::Debug for CaptureClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaptureClient")
    }
}

/// The process-wide client to the shared capture daemon. ONE per nova process
/// — never per session/tool: every extra ScreenCaptureKit-touching connection
/// from the same binary is a storm participant.
pub fn shared_client() -> &'static CaptureClient {
    static CLIENT: OnceLock<CaptureClient> = OnceLock::new();
    CLIENT.get_or_init(CaptureClient::new)
}

impl CaptureClient {
    pub fn new() -> Self {
        // Recovery actions (spawns, kills, scorched earth) must be traceable
        // after the fact regardless of the MCP host's log handling — they go
        // to the same step-trace file the daemon writes. Cheap: client
        // processes only step() on those rare events, never per capture.
        crate::capture::screenshot::enable_step_trace();
        Self::default()
    }

    /// Capture pixels via the shared daemon. See [`Self::request`].
    pub fn capture(&self, req: &CaptureRequest) -> Result<RawCapture, String> {
        match self.request(req)? {
            Reply::Image(raw) => Ok(raw),
            Reply::Windows(_) => Err("daemon sent a windows reply to a capture request".into()),
        }
    }

    /// Enumerate on-screen windows via the shared daemon (frontmost first).
    pub fn windows(&self) -> Result<Vec<WireWindow>, String> {
        match self.request(&CaptureRequest::Windows)? {
            Reply::Windows(w) => Ok(w),
            Reply::Image(_) => Err("daemon sent pixels to a windows request".into()),
        }
    }

    /// Send one request to the shared daemon, transparently spawning/replacing
    /// it as needed. Blocking — call from `spawn_blocking`. Worst case (full
    /// recovery ladder against a hard wedge) ≈ 40s; the healthy path is
    /// sub-second.
    ///
    /// Recovery ladder on wedge-class failures:
    ///   1. retry against a freshly spawned daemon (the old one is killed —
    ///      a daemon that wedged has a stuck ScreenCaptureKit thread and must
    ///      die to free its replayd state);
    ///   2. kill EVERY stray capture process (other daemons, legacy workers —
    ///      all expendable by contract) and `killall -9 replayd` (it ignores
    ///      SIGTERM!), then one final retry.
    pub fn request(&self, req: &CaptureRequest) -> Result<Reply, String> {
        let mut guard = self
            .io
            .lock()
            .map_err(|_| "capture client lock poisoned".to_string())?;
        let mut notes: Vec<String> = Vec::new();
        let mut frame_retry_used = false;
        let mut rung = 0u32;
        while rung < 3 {
            match self.attempt(&mut guard, req) {
                Ok(reply) => return Ok(reply),
                Err(AttemptError::Clean(e)) => {
                    // One free retry for a stream stall ("produced no frame"):
                    // the daemon already dropped its stream and rebuilds on the
                    // next request, so an immediate retry usually succeeds.
                    if !frame_retry_used && e.contains("produced no frame") {
                        frame_retry_used = true;
                        step(&format!("client: stream stall, retrying once ({e})"));
                        notes.push(format!("stream stall, retried once ({e})"));
                        continue;
                    }
                    return Err(if notes.is_empty() {
                        e
                    } else {
                        format!("{e} (after: {})", notes.join("; "))
                    });
                }
                Err(AttemptError::Fatal(e)) => {
                    return Err(if notes.is_empty() {
                        e
                    } else {
                        format!("{e} (after: {})", notes.join("; "))
                    });
                }
                Err(AttemptError::Wedge(e)) => {
                    let failed_pid = guard.as_ref().map(|c| c.daemon_pid);
                    *guard = None;
                    notes.push(e);
                    rung += 1;
                    if rung >= 3 {
                        continue; // out of rungs — the while condition ends the loop
                    }
                    // Serialize recovery across ALL clients (every connected
                    // nova hits the wedge at once when the daemon dies): the
                    // first one in performs the rung; the rest wait here, then
                    // discover a healthy daemon and skip their kills — without
                    // this, a laggard's rung-2 scorched earth would destroy
                    // the daemon its peers just recovered onto.
                    let _recovery = RecoveryLock::acquire();
                    if healthy_daemon_answers() {
                        step("client: a peer already recovered the daemon — retrying without kills");
                        notes.push("peer recovered the daemon; retried".to_string());
                        continue;
                    }
                    match rung {
                        1 => {
                            // Prefer the pid of the daemon that actually failed
                            // us; fall back to the lock file for one that never
                            // completed a handshake. Either may be stale (pid
                            // reused), so verify before SIGKILLing.
                            let pid = failed_pid
                                .or_else(lockfile_pid)
                                .filter(|p| is_capture_daemon(*p));
                            if let Some(pid) = pid {
                                step(&format!("client: killing wedged daemon pid={pid}"));
                                kill_pid(pid);
                                notes.push(format!("killed wedged daemon (pid {pid})"));
                            }
                            // Let the flock release + replayd notice the death.
                            std::thread::sleep(Duration::from_millis(400));
                        }
                        2 => scorched_earth(&mut notes),
                        _ => {}
                    }
                }
            }
        }
        Err(format!(
            "capture failed after the full recovery ladder: {}",
            notes.join(" → ")
        ))
    }

    /// Drop the connection (next capture reconnects). Used by the server's
    /// outer-timeout backstop; safe to call while a capture is in flight (the
    /// in-flight one keeps its handles and times out on its own).
    pub fn disconnect(&self) {
        if let Ok(mut io) = self.io.try_lock() {
            *io = None;
        }
    }

    /// Daemon pid of the live connection, if any (tests/diagnostics).
    pub fn daemon_pid(&self) -> Option<i32> {
        self.io
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.daemon_pid))
    }

    fn attempt(
        &self,
        guard: &mut Option<Conn>,
        req: &CaptureRequest,
    ) -> Result<Reply, AttemptError> {
        use AttemptError::Wedge;
        if guard.is_none() {
            *guard = Some(connect_or_spawn()?);
        }
        let conn = guard.as_mut().unwrap();
        let line = serde_json::to_string(req).map_err(|e| Wedge(format!("serialize: {e}")))?;
        conn.writer
            .write_all(line.as_bytes())
            .and_then(|_| conn.writer.write_all(b"\n"))
            .and_then(|_| conn.writer.flush())
            .map_err(|e| Wedge(format!("write to capture daemon: {e}")))?;

        let mut header_line = String::new();
        match conn.reader.read_line(&mut header_line) {
            Ok(0) => {
                return Err(Wedge(
                    "capture daemon closed the connection mid-request (it restarted to clear a \
                     wedge, or was killed)"
                        .to_string(),
                ))
            }
            Ok(_) => {}
            Err(e) => {
                return Err(Wedge(format!(
                    "capture daemon did not respond within {CLIENT_READ_TIMEOUT:?} ({e})"
                )))
            }
        }
        let header: Header = serde_json::from_str(header_line.trim())
            .map_err(|e| Wedge(format!("bad daemon header: {e}")))?;
        if !header.ok {
            let msg = header.error.unwrap_or_else(|| "capture failed".to_string());
            return Err(if header.wedge {
                AttemptError::Wedge(msg)
            } else {
                AttemptError::Clean(msg)
            });
        }
        if let Some(windows) = header.windows {
            return Ok(Reply::Windows(windows));
        }
        let mut buf = vec![0u8; header.len];
        conn.reader
            .read_exact(&mut buf)
            .map_err(|e| Wedge(format!("read capture body: {e}")))?;
        let image = image::RgbImage::from_raw(header.width, header.height, buf)
            .ok_or_else(|| Wedge("daemon returned a mismatched image buffer".to_string()))?;
        Ok(Reply::Image(RawCapture {
            image,
            view: ViewFrame {
                origin: header.origin,
                region: header.region,
                screenshot: header.screenshot,
            },
            window_pid: header.window_pid,
        }))
    }
}

/// Connect to the daemon, spawning it if absent, and validate the handshake.
/// Replaces a stale daemon (proto/build mismatch) by killing it.
fn connect_or_spawn() -> Result<Conn, AttemptError> {
    use AttemptError::{Fatal, Wedge};
    let sock = socket_path();
    let deadline = Instant::now() + CONNECT_BUDGET;
    let mut spawned = false;
    loop {
        match UnixStream::connect(&sock) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
                let mut reader = BufReader::new(
                    s.try_clone()
                        .map_err(|e| Wedge(format!("clone socket: {e}")))?,
                );
                let mut hello_line = String::new();
                if reader.read_line(&mut hello_line).unwrap_or(0) == 0 {
                    // Daemon died between accept and hello; retry.
                    if Instant::now() >= deadline {
                        return Err(Wedge(
                            "capture daemon keeps dying during handshake".to_string(),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let hello: Hello = serde_json::from_str(hello_line.trim())
                    .map_err(|e| Wedge(format!("bad daemon hello: {e}")))?;
                // Daemon speaks a NEWER proto: WE are the stale ones. Killing
                // it would only respawn the same newer binary from disk — an
                // unwinnable kill/respawn loop that assassinates the daemon
                // current clients depend on. Fail this client; Fatal so the
                // ladder doesn't escalate to kills either.
                if hello.proto > PROTO_VERSION {
                    return Err(Fatal(format!(
                        "capture daemon (pid {}) speaks proto {}, but this nova process was \
                         built for proto {PROTO_VERSION}; restart this nova process to pick \
                         up the new binary",
                        hello.pid, hello.proto
                    )));
                }
                // A daemon whose responsibility chain lacks the Screen
                // Recording grant fails every capture forever; if WE hold the
                // grant, replace it so the respawn inherits our granted chain.
                let tcc_stale = !hello.preflight
                    && crate::display::geometry::preflight_screen_capture();
                if hello.proto < PROTO_VERSION
                    || hello.exe_mtime_ms != exe_mtime_ms()
                    || tcc_stale
                {
                    step(&format!(
                        "client: daemon pid={} is stale (proto {} vs {PROTO_VERSION}, build {} \
                         vs {}, preflight={}) — replacing it",
                        hello.pid,
                        hello.proto,
                        hello.exe_mtime_ms,
                        exe_mtime_ms(),
                        hello.preflight,
                    ));
                    kill_pid(hello.pid);
                    drop(reader);
                    std::thread::sleep(Duration::from_millis(300));
                    spawned = false; // respawn the current build below
                    continue;
                }
                let _ = s.set_read_timeout(Some(CLIENT_READ_TIMEOUT));
                return Ok(Conn {
                    reader,
                    writer: s,
                    daemon_pid: hello.pid,
                });
            }
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(Wedge(format!(
                        "cannot reach capture daemon at {} within {CONNECT_BUDGET:?}: {e}",
                        sock.display()
                    )));
                }
                if !spawned {
                    spawn_daemon().map_err(Fatal)?;
                    spawned = true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Spawn the daemon, detached into its own session so terminal signals to the
/// spawning nova never reach it. Children are reaped lazily on later spawns.
fn spawn_daemon() -> Result<(), String> {
    static SPAWNED: OnceLock<Mutex<Vec<Child>>> = OnceLock::new();
    let registry = SPAWNED.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut v) = registry.lock() {
        v.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    }

    let exe = capture_bin().ok_or("cannot determine nova binary path")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/nova-capture-daemon.log")
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut cmd = Command::new(&exe);
    cmd.arg("--capture-daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid in the forked child, before exec — async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn capture daemon ({}): {e}", exe.display()))?;
    step(&format!("client: spawned capture daemon pid={}", child.id()));
    // A daemon that dies within its first instants is one of: lost election
    // (exit 0 — benign, another daemon is serving), or a binary that cannot run
    // `--capture-daemon` at all (e.g. a test harness without NOVA_CAPTURE_BIN)
    // — FATAL, because retrying/escalating can never produce a daemon.
    std::thread::sleep(Duration::from_millis(250));
    if let Ok(Some(status)) = child.try_wait() {
        if !status.success() {
            return Err(format!(
                "capture daemon ({}) exited immediately ({status}) — either this binary \
                 doesn't support --capture-daemon (when embedding or testing, point \
                 NOVA_CAPTURE_BIN at the nova binary) or it failed to start; check \
                 /tmp/nova-capture-daemon.log",
                exe.display()
            ));
        }
    } else if let Ok(mut v) = registry.lock() {
        v.push(child);
    }
    Ok(())
}

/// True if `pid` is currently a live `--capture-daemon` process — guards the
/// lockfile-pid kill against stale pids that the OS has reused.
fn is_capture_daemon(pid: i32) -> bool {
    Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("--capture-daemon"))
        .unwrap_or(false)
}

/// Cross-process mutex for the recovery ladder (flock on `<lock>.recovery`).
/// Waits up to 30s for a peer's ladder to finish; on timeout proceeds
/// unlocked (better a rare double-recovery than a capture stuck forever
/// behind a crashed peer — flocks die with their holder, so that's rare).
struct RecoveryLock(#[allow(dead_code)] Option<std::fs::File>);

impl RecoveryLock {
    fn acquire() -> Self {
        use std::os::fd::AsRawFd;
        let mut path = lock_path().into_os_string();
        path.push(".recovery");
        let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
        else {
            return Self(None);
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            // SAFETY: flock on an fd we own; released on drop/close.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Self(Some(f));
            }
            if Instant::now() >= deadline {
                step("client: recovery lock still held after 30s — proceeding unlocked");
                return Self(None);
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// Quick probe: does a CURRENT-build daemon answer a valid handshake right
/// now? Used after waiting on the recovery lock — true means a peer already
/// recovered and our kills would be friendly fire.
fn healthy_daemon_answers() -> bool {
    let Ok(s) = UnixStream::connect(socket_path()) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    if BufReader::new(s).read_line(&mut line).unwrap_or(0) == 0 {
        return false;
    }
    serde_json::from_str::<Hello>(line.trim())
        .map(|h| h.proto == PROTO_VERSION && h.exe_mtime_ms == exe_mtime_ms())
        .unwrap_or(false)
}

/// Pid recorded in the lock file by the live daemon (None if absent/garbage).
fn lockfile_pid() -> Option<i32> {
    std::fs::read_to_string(lock_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Pid of the live capture daemon, verified to actually BE one (guards against
/// stale lockfiles / pid reuse). For diagnostics (`--selftest`).
pub fn live_daemon_pid() -> Option<i32> {
    lockfile_pid().filter(|p| is_capture_daemon(*p))
}

/// Pid of ANY live `--capture-daemon` process — including ones on other
/// sockets (e.g. stray test daemons, same binary path!). Any of them collides
/// with a second same-binary ScreenCaptureKit client, so diagnostics that
/// stream directly must skip when one exists.
pub fn any_capture_daemon_pid() -> Option<i32> {
    let me = std::process::id() as i32;
    let out = Command::new("/usr/bin/pgrep")
        .args(["-f", "--", "--capture-daemon"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|s| s.parse::<i32>().ok())
        .find(|p| *p != me)
}

fn kill_pid(pid: i32) {
    if pid > 1 {
        // SAFETY: sending SIGKILL to a specific non-init pid we manage.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// Rung 2 of the recovery ladder: kill every capture process of this binary
/// (daemons, legacy workers, proxies — all expendable and respawned on demand;
/// they are the only processes that hold nova-side replayd streams), then
/// bounce replayd itself. SIGTERM is useless against replayd — it ignores it —
/// so this is `killall -9`. Order matters: clients first, daemon second, or the
/// survivors re-wedge the fresh replayd by reconnecting.
fn scorched_earth(notes: &mut Vec<String>) {
    // In a private namespace (NOVA_CAPTURE_SOCK set — tests, embedders) the
    // sweep must NOT touch the user's real capture processes or replayd; the
    // namespace daemon was already handled by rung 1.
    if std::env::var_os("NOVA_CAPTURE_SOCK").is_some() {
        step("client: scorched earth skipped (private NOVA_CAPTURE_SOCK namespace)");
        notes.push("scorched earth skipped (private capture namespace)".to_string());
        std::thread::sleep(Duration::from_millis(300));
        return;
    }
    let me = std::process::id() as i32;
    let mut killed = Vec::new();
    if let Ok(out) = Command::new("/usr/bin/pgrep")
        .args(["-f", "--", "--capture-(daemon|worker)"])
        .output()
    {
        for pid in String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|s| s.parse::<i32>().ok())
        {
            if pid != me {
                kill_pid(pid);
                killed.push(pid);
            }
        }
    }
    let replayd = Command::new("/usr/bin/killall")
        .args(["-9", "replayd"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    step(&format!(
        "client: scorched earth — killed capture procs {killed:?}, replayd bounced={replayd}"
    ));
    notes.push(format!(
        "killed all capture processes {killed:?} and SIGKILLed replayd (ok={replayd})"
    ));
    std::thread::sleep(Duration::from_millis(600));
}
