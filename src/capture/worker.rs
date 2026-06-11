//! Capture worker subprocess — isolates the hang-prone ScreenCaptureKit call.
//!
//! ScreenCaptureKit's `capture_image` can hang indefinitely (a busy window-server
//! session, a pathological window), and a hung capture wedges `replayd`
//! system-wide so EVERY later capture hangs too — recoverable only by killing the
//! process that holds the stuck stream. A tokio timeout cannot cancel a blocking
//! thread, so an in-process capture that hangs leaks a stuck thread which keeps
//! re-wedging replayd.
//!
//! So the raw SCK capture runs in a dedicated child process (`nova
//! --capture-worker`). If a capture hangs, the server KILLS the child — freeing
//! its stuck stream and letting replayd recover — and respawns a fresh worker on
//! the next call. The marks/Accessibility walk stays in the server process (its
//! live AX handles can't cross a process boundary); it runs only after the raw
//! image comes back, via [`crate::capture::screenshot::finish_capture`].

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::capture::screenshot::{
    capture_display_raw, capture_region_raw, capture_window_raw, RawCapture,
};
use crate::display::view::ViewFrame;

/// What to capture. Sent as one JSON line to the worker's stdin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CaptureRequest {
    Display,
    Window { query: String },
    Region { rect: (f64, f64, f64, f64) },
}

/// Response header (one JSON line). When `ok`, exactly `len` raw RGB8 bytes
/// follow it on the pipe.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    ok: bool,
    error: Option<String>,
    width: u32,
    height: u32,
    origin: (f64, f64),
    region: (f64, f64),
    screenshot: (f64, f64),
    window_pid: Option<i32>,
    len: usize,
}

// ── Worker side (child process) ─────────────────────────────────────

/// Run the worker loop: read one JSON request per line from stdin, capture, and
/// write a JSON header line + raw RGB bytes to stdout. Exits on stdin EOF (the
/// parent went away). Logs go to stderr; stdout carries only the binary protocol.
pub fn run() -> ! {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => std::process::exit(0), // parent closed stdin
            Ok(_) => {}
            Err(_) => std::process::exit(0),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let result = match serde_json::from_str::<CaptureRequest>(trimmed) {
            Ok(req) => do_capture(req),
            Err(e) => Err(format!("bad request: {e}")),
        };
        let mut out = stdout.lock();
        match result {
            Ok(raw) => {
                let header = Header {
                    ok: true,
                    error: None,
                    width: raw.image.width(),
                    height: raw.image.height(),
                    origin: raw.view.origin,
                    region: raw.view.region,
                    screenshot: raw.view.screenshot,
                    window_pid: raw.window_pid,
                    len: raw.image.as_raw().len(),
                };
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&header).unwrap_or_default()
                );
                let _ = out.write_all(raw.image.as_raw());
                let _ = out.flush();
            }
            Err(e) => {
                let header = Header {
                    ok: false,
                    error: Some(e),
                    width: 0,
                    height: 0,
                    origin: (0.0, 0.0),
                    region: (0.0, 0.0),
                    screenshot: (0.0, 0.0),
                    window_pid: None,
                    len: 0,
                };
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&header).unwrap_or_default()
                );
                let _ = out.flush();
            }
        }
    }
}

fn do_capture(req: CaptureRequest) -> Result<RawCapture, String> {
    match req {
        CaptureRequest::Display => capture_display_raw(),
        CaptureRequest::Window { query } => capture_window_raw(&query),
        CaptureRequest::Region { rect } => capture_region_raw(rect),
    }
}

// ── Server side (parent process) ────────────────────────────────────

#[derive(Debug)]
struct WorkerIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// A persistent, killable capture worker. The raw SCK capture runs in the child;
/// on a hang the server [`kill`](CaptureWorker::kill)s the child (freeing the
/// stuck stream) and the next [`capture`](CaptureWorker::capture) respawns one.
///
/// The IO handles and the kill handle live behind SEPARATE locks on purpose:
/// `capture` holds the IO lock while blocked on the child's response, and
/// `kill` (called from the timeout path on another task) needs only the child
/// handle — killing the child closes the pipe, which unblocks that read.
#[derive(Debug, Default)]
pub struct CaptureWorker {
    io: Mutex<Option<WorkerIo>>,
    child: Mutex<Option<Child>>,
}

impl CaptureWorker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture, spawning the worker if needed. Blocking — call from
    /// `spawn_blocking`. On any IO error the worker is dropped so the next call
    /// respawns a fresh one.
    pub fn capture(&self, req: &CaptureRequest) -> Result<RawCapture, String> {
        let mut guard = self
            .io
            .lock()
            .map_err(|_| "capture worker lock poisoned".to_string())?;
        if guard.is_none() {
            *guard = Some(self.spawn()?);
        }
        let io = guard.as_mut().unwrap();
        match Self::exchange(io, req) {
            Ok(raw) => Ok(raw),
            Err(e) => {
                *guard = None; // streams are broken
                self.reap();
                Err(e)
            }
        }
    }

    /// Kill the worker process — call this when a capture times out. Safe to call
    /// from a different task than the one blocked in [`capture`]: it kills the
    /// child (closing its stdout, which unblocks the read in `capture`) and then
    /// clears the IO handle so the next capture respawns a fresh worker.
    ///
    /// The IO clear is a `try_lock`: if a capture is currently blocked holding the
    /// IO lock (the hung-capture case), this skips it — that capture's own read
    /// errors out on the closed pipe and clears the handle itself. If no capture
    /// is in flight (e.g. recovering proactively), it clears it here so the very
    /// next capture respawns immediately rather than failing once on a dead pipe.
    pub fn kill(&self) {
        self.reap();
        if let Ok(mut io) = self.io.try_lock() {
            *io = None;
        }
    }

    fn reap(&self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    fn spawn(&self) -> Result<WorkerIo, String> {
        // The worker is this same binary re-invoked with `--capture-worker`.
        // `NOVA_CAPTURE_WORKER_BIN` overrides the path (used by tests, which run
        // under a different executable than the nova binary).
        let exe = match std::env::var_os("NOVA_CAPTURE_WORKER_BIN") {
            Some(p) => std::path::PathBuf::from(p),
            None => std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?,
        };
        let mut child = Command::new(exe)
            .arg("--capture-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn capture worker: {e}"))?;
        let stdin = child.stdin.take().ok_or("worker has no stdin")?;
        let stdout = child.stdout.take().ok_or("worker has no stdout")?;
        *self
            .child
            .lock()
            .map_err(|_| "child lock poisoned".to_string())? = Some(child);
        Ok(WorkerIo {
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn exchange(io: &mut WorkerIo, req: &CaptureRequest) -> Result<RawCapture, String> {
        let line = serde_json::to_string(req).map_err(|e| format!("serialize request: {e}"))?;
        io.stdin
            .write_all(line.as_bytes())
            .and_then(|_| io.stdin.write_all(b"\n"))
            .and_then(|_| io.stdin.flush())
            .map_err(|e| format!("write to capture worker: {e}"))?;

        let mut header_line = String::new();
        if io
            .stdout
            .read_line(&mut header_line)
            .map_err(|e| format!("read from capture worker: {e}"))?
            == 0
        {
            return Err("capture worker exited before responding".to_string());
        }
        let header: Header = serde_json::from_str(header_line.trim())
            .map_err(|e| format!("bad capture worker header: {e}"))?;
        if !header.ok {
            return Err(header.error.unwrap_or_else(|| "capture failed".to_string()));
        }
        let mut buf = vec![0u8; header.len];
        io.stdout
            .read_exact(&mut buf)
            .map_err(|e| format!("read capture body: {e}"))?;
        let image = image::RgbImage::from_raw(header.width, header.height, buf)
            .ok_or("capture worker returned a mismatched image buffer")?;
        Ok(RawCapture {
            image,
            view: ViewFrame {
                origin: header.origin,
                region: header.region,
                screenshot: header.screenshot,
            },
            window_pid: header.window_pid,
        })
    }
}
