use crate::framing::{
    encode_native, encode_ndjson, NativeDecoder, NdjsonDecoder, MAX_MESSAGE_BYTES,
};
use crate::protocol::{
    host_hello, redacted_diagnostic, validate_extension_origin, validate_message,
};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A same-UID peer must not be able to pin Nova.app's single Chrome broker by
/// sending only part of a frame or by ceasing to read. Native messaging is a
/// local hop, so a complete bounded message should finish comfortably within
/// this limit.
const STREAM_FRAME_TIMEOUT: Duration = Duration::from_millis(500);

pub fn default_socket_path() -> PathBuf {
    PathBuf::from(format!("/tmp/nova-app-{}/chrome.sock", effective_uid()))
}

pub(crate) fn configured_socket_path() -> Result<PathBuf> {
    let path = env::var_os("NOVA_CHROME_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path);
    if !path.is_absolute() || path.file_name().and_then(|name| name.to_str()) != Some("chrome.sock")
    {
        bail!("NOVA_CHROME_SOCKET must be an absolute path ending in chrome.sock");
    }
    Ok(path)
}

pub struct AppBridgeListener {
    listener: UnixListener,
    path: PathBuf,
    identity: (u64, u64),
}

impl AppBridgeListener {
    /// Bind a private app-side bridge. The parent directory is required to be
    /// owned by the effective user and inaccessible to group/other users.
    pub fn bind(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_absolute()
            || path.file_name().and_then(|name| name.to_str()) != Some("chrome.sock")
        {
            bail!("bridge socket path must be absolute and end in chrome.sock");
        }
        let parent = path.parent().context("bridge socket has no parent")?;
        ensure_private_parent(parent)?;
        remove_owned_stale_socket(path)?;

        let listener = UnixListener::bind(path)
            .with_context(|| format!("bind Chrome bridge at {}", path.display()))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .context("set Chrome bridge socket permissions")?;
        let metadata = fs::symlink_metadata(path).context("stat bound Chrome bridge socket")?;
        validate_socket_metadata(&metadata)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    pub fn accept(&self) -> Result<AppBridgeConnection> {
        let (stream, _) = self
            .listener
            .accept()
            .context("accept Chrome native host")?;
        verify_peer_uid(&stream)?;
        AppBridgeConnection::new(stream)
    }

    /// Configure blocking behavior for both [`accept`](Self::accept) and
    /// [`try_accept`](Self::try_accept). The app-side broker uses a
    /// non-blocking listener so it can service bounded MCP requests while no
    /// native host is connected.
    pub fn set_nonblocking(&self, nonblocking: bool) -> Result<()> {
        self.listener
            .set_nonblocking(nonblocking)
            .context("configure Chrome bridge listener")
    }

    /// Accept one authenticated native host without waiting.
    ///
    /// This returns `Ok(None)` only when a non-blocking listener has no queued
    /// connection. Every accepted stream still passes the same-UID check
    /// before it is exposed to the app broker.
    pub fn try_accept(&self) -> Result<Option<AppBridgeConnection>> {
        match self.listener.accept() {
            Ok((stream, _)) => {
                verify_peer_uid(&stream)?;
                AppBridgeConnection::new(stream).map(Some)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error).context("accept Chrome native host"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AppBridgeListener {
    fn drop(&mut self) {
        // Do not unlink a path that another process replaced after this listener
        // started. Device+inode comparison makes cleanup race-safe.
        if let Ok(metadata) = fs::symlink_metadata(&self.path) {
            if (metadata.dev(), metadata.ino()) == self.identity {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

pub struct AppBridgeConnection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl AppBridgeConnection {
    fn new(stream: UnixStream) -> Result<Self> {
        // Listener non-blocking state is not specified to propagate uniformly
        // to accepted sockets across supported Unix platforms. Establish the
        // connection invariant explicitly; send() temporarily switches its
        // clone to non-blocking mode while enforcing an absolute deadline.
        stream
            .set_nonblocking(false)
            .context("configure blocking Chrome bridge stream")?;
        stream
            .set_read_timeout(Some(STREAM_FRAME_TIMEOUT))
            .context("set Chrome bridge read timeout")?;
        stream
            .set_write_timeout(Some(STREAM_FRAME_TIMEOUT))
            .context("set Chrome bridge write timeout")?;
        let writer = stream.try_clone().context("clone Chrome bridge stream")?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
        })
    }

    pub fn receive(&mut self) -> Result<Option<Value>> {
        let deadline = Instant::now() + STREAM_FRAME_TIMEOUT;
        let Some(line) = read_line_limited_until(&mut self.reader, deadline)? else {
            return Ok(None);
        };
        let value: Value = serde_json::from_slice(&line).context("invalid Chrome bridge JSON")?;
        validate_message(&value)?;
        Ok(Some(value))
    }

    pub fn send(&mut self, value: &Value) -> Result<()> {
        validate_message(value)?;
        let frame = encode_ndjson(value)?;
        let deadline = Instant::now() + STREAM_FRAME_TIMEOUT;
        write_all_until(&mut self.writer, &frame, deadline)
    }

    /// Wait until a read would make progress, the peer has disconnected, or
    /// the timeout expires. This keeps the app broker responsive without
    /// weakening the bounded NDJSON decoder in [`receive`](Self::receive).
    pub fn wait_readable(&self, timeout: Duration) -> Result<bool> {
        // BufReader may have read past the previous newline. In that case the
        // next complete message is already in userspace and polling the fd can
        // incorrectly report no kernel data until some unrelated later write.
        if !self.reader.buffer().is_empty() {
            return Ok(true);
        }
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: self.reader.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: descriptor points to one initialized pollfd for the
            // duration of the call, and its fd belongs to this connection.
            let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if ready > 0 {
                if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                    bail!("Chrome bridge connection failed");
                }
                // POLLHUP is readable for our purposes: receive() will either
                // drain a final message or report a clean EOF.
                return Ok(descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0);
            }
            if ready == 0 {
                return Ok(false);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).context("poll Chrome native host");
            }
        }
    }
}

pub(crate) fn run_host() -> Result<()> {
    let origin = env::args()
        .nth(1)
        .context("Chrome did not provide the extension origin")?;
    let extension_id = validate_extension_origin(&origin)?;
    let path = configured_socket_path()?;
    let mut app = connect_verified(&path)?;
    let hello = host_hello(&extension_id)?;
    app.write_all(&encode_ndjson(&hello)?)
        .context("send host hello to Nova.app")?;
    app.flush().context("flush host hello")?;
    relay(app)
}

fn relay(mut app: UnixStream) -> Result<()> {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut native_decoder = NativeDecoder::default();
    let mut app_decoder = NdjsonDecoder::default();
    let stdin_fd = stdin.as_raw_fd();
    let app_fd = app.as_raw_fd();
    let mut native_buffer = [0_u8; 16 * 1024];
    let mut app_buffer = [0_u8; 16 * 1024];

    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: app_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = loop {
            // SAFETY: descriptors points to two initialized pollfd values for
            // the duration of this blocking call.
            let result =
                unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
            if result >= 0 {
                break result;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error).context("poll Chrome bridge streams");
            }
        };
        if ready == 0 {
            continue;
        }

        if descriptors[0].revents & libc::POLLIN != 0 {
            let read = stdin
                .read(&mut native_buffer)
                .context("read Chrome native message")?;
            if read == 0 {
                native_decoder.finish()?;
                return Ok(());
            }
            for message in native_decoder.push(&native_buffer[..read])? {
                validate_message(&message).context("reject app-bound protocol message")?;
                log_diagnostic("extension_to_app", &message);
                app.write_all(&encode_ndjson(&message)?)
                    .context("forward message to Nova.app")?;
                app.flush().context("flush Nova.app bridge")?;
            }
        }

        if descriptors[1].revents & libc::POLLIN != 0 {
            let read = app.read(&mut app_buffer).context("read Nova.app bridge")?;
            if read == 0 {
                app_decoder.finish()?;
                return Ok(());
            }
            for message in app_decoder.push(&app_buffer[..read])? {
                validate_message(&message).context("reject extension-bound protocol message")?;
                log_diagnostic("app_to_extension", &message);
                stdout
                    .write_all(&encode_native(&message)?)
                    .context("forward native message to Chrome")?;
                stdout.flush().context("flush Chrome native message")?;
            }
        }

        for descriptor in descriptors {
            if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                bail!("Chrome bridge stream failed");
            }
            if descriptor.revents & libc::POLLHUP != 0 && descriptor.revents & libc::POLLIN == 0 {
                return Ok(());
            }
        }
    }
}

fn log_diagnostic(direction: &str, value: &Value) {
    match redacted_diagnostic(value) {
        Ok(diagnostic) => eprintln!("nova-chrome-host {direction}: {diagnostic}"),
        Err(_) => eprintln!("nova-chrome-host {direction}: invalid message"),
    }
}

fn connect_verified(path: &Path) -> Result<UnixStream> {
    let parent = path
        .parent()
        .context("Chrome bridge socket has no parent")?;
    validate_private_parent(parent)?;
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "Nova.app Chrome socket is unavailable at {}",
            path.display()
        )
    })?;
    validate_socket_metadata(&metadata)?;
    let stream = UnixStream::connect(path)
        .with_context(|| format!("connect to Nova.app Chrome socket at {}", path.display()))?;
    verify_peer_uid(&stream)?;
    Ok(stream)
}

fn ensure_private_parent(parent: &Path) -> Result<()> {
    match fs::symlink_metadata(parent) {
        Ok(_) => validate_private_parent(parent),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(parent)
                .with_context(|| format!("create private bridge directory {}", parent.display()))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .context("set bridge directory permissions")?;
            validate_private_parent(parent)
        }
        Err(error) => Err(error).context("stat bridge directory"),
    }
}

fn validate_private_parent(parent: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(parent).context("stat bridge directory")?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!("bridge parent is not a real directory");
    }
    if metadata.uid() != effective_uid() {
        bail!("bridge parent is not owned by the current user");
    }
    if metadata.mode() & 0o077 != 0 {
        bail!("bridge parent permissions must exclude group and other users");
    }
    Ok(())
}

fn validate_socket_metadata(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_socket() || metadata.file_type().is_symlink() {
        bail!("Chrome bridge path is not a Unix socket");
    }
    if metadata.uid() != effective_uid() {
        bail!("Chrome bridge socket is not owned by the current user");
    }
    if metadata.mode() & 0o077 != 0 {
        bail!("Chrome bridge socket permissions must exclude group and other users");
    }
    Ok(())
}

fn remove_owned_stale_socket(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("stat existing bridge path"),
    };
    validate_socket_metadata(&metadata)?;
    match UnixStream::connect(path) {
        Ok(_) => bail!("Chrome bridge is already active"),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path).context("remove owned stale Chrome bridge socket")
        }
        Err(error) => Err(error).context("probe existing Chrome bridge socket"),
    }
}

fn remaining_until(deadline: Instant, operation: &str) -> Result<Duration> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        bail!("Chrome bridge {operation} deadline exceeded");
    };
    if remaining.is_zero() {
        bail!("Chrome bridge {operation} deadline exceeded");
    }
    Ok(remaining)
}

fn read_line_limited_until(
    reader: &mut BufReader<UnixStream>,
    deadline: Instant,
) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = loop {
            let remaining = remaining_until(deadline, "receive")?;
            reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .context("set Chrome bridge frame read timeout")?;
            match reader.fill_buf() {
                Ok(available) => break available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    bail!("Chrome bridge receive deadline exceeded")
                }
                Err(error) => return Err(error).context("read Chrome bridge message"),
            }
        };
        remaining_until(deadline, "receive")?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            bail!("truncated Chrome bridge message");
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len() + take > MAX_MESSAGE_BYTES + 1 {
            bail!("Chrome bridge message exceeds {MAX_MESSAGE_BYTES} bytes");
        }
        line.extend_from_slice(&available[..take]);
        let ended = available[take - 1] == b'\n';
        reader.consume(take);
        if ended {
            remaining_until(deadline, "receive")?;
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                bail!("empty Chrome bridge message");
            }
            return Ok(Some(line));
        }
    }
}

fn write_all_until(writer: &mut UnixStream, frame: &[u8], deadline: Instant) -> Result<()> {
    writer
        .set_nonblocking(true)
        .context("configure non-blocking Chrome bridge send")?;
    let result = write_all_nonblocking_until(writer, frame, deadline);
    let restore = writer
        .set_nonblocking(false)
        .context("restore blocking Chrome bridge stream");
    match (result, restore) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(restore_error)) => {
            Err(error.context(format!("also failed to restore stream: {restore_error}")))
        }
    }
}

fn write_all_nonblocking_until(
    writer: &mut UnixStream,
    mut frame: &[u8],
    deadline: Instant,
) -> Result<()> {
    while !frame.is_empty() {
        let remaining = remaining_until(deadline, "send")?;
        writer
            .set_write_timeout(Some(remaining))
            .context("set Chrome bridge frame write timeout")?;
        match writer.write(frame) {
            Ok(0) => bail!("Chrome bridge connection closed while sending a frame"),
            Ok(written) => frame = &frame[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                wait_writable_until(writer, deadline)?;
            }
            Err(error) => return Err(error).context("write Chrome bridge message"),
        }
    }

    loop {
        let remaining = remaining_until(deadline, "send")?;
        writer
            .set_write_timeout(Some(remaining))
            .context("set Chrome bridge frame flush timeout")?;
        match writer.flush() {
            Ok(()) => {
                remaining_until(deadline, "send")?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                wait_writable_until(writer, deadline)?;
            }
            Err(error) => return Err(error).context("flush Chrome bridge message"),
        }
    }
}

fn wait_writable_until(writer: &UnixStream, deadline: Instant) -> Result<()> {
    loop {
        let remaining = remaining_until(deadline, "send")?;
        let timeout_ms = remaining
            .as_millis()
            .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: writer.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the call,
        // and its fd belongs to this connection.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready > 0 {
            if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                bail!("Chrome bridge connection failed while sending a frame");
            }
            if descriptor.revents & libc::POLLOUT != 0 {
                return Ok(());
            }
            continue;
        }
        if ready == 0 {
            bail!("Chrome bridge send deadline exceeded");
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error).context("poll Chrome bridge writer");
        }
    }
}

#[cfg(test)]
fn read_line_limited<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().context("read Chrome bridge message")?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            bail!("truncated Chrome bridge message");
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len() + take > MAX_MESSAGE_BYTES + 1 {
            bail!("Chrome bridge message exceeds {MAX_MESSAGE_BYTES} bytes");
        }
        line.extend_from_slice(&available[..take]);
        let ended = available[take - 1] == b'\n';
        reader.consume(take);
        if ended {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                bail!("empty Chrome bridge message");
            }
            return Ok(Some(line));
        }
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and no side effects.
    unsafe { libc::geteuid() }
}

fn verify_peer_uid(stream: &UnixStream) -> Result<()> {
    let peer = peer_uid(stream.as_raw_fd())?;
    if peer != effective_uid() {
        bail!("Chrome bridge peer UID does not match the current user");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn peer_uid(fd: RawFd) -> Result<u32> {
    let mut uid = 0_u32;
    let mut gid = 0_u32;
    // SAFETY: fd is a live UnixStream descriptor; uid/gid are valid outputs.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error()).context("get Chrome bridge peer credentials");
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(fd: RawFd) -> Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: fd is a live UnixStream and credentials/length are valid outputs.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error()).context("get Chrome bridge peer credentials");
    }
    Ok(credentials.uid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_uid(_fd: RawFd) -> Result<u32> {
    bail!("peer credential verification is unavailable on this Unix platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Cursor, Write};
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "nova-chrome-bridge-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self { path }
        }

        fn socket_path(&self) -> PathBuf {
            self.path.join("chrome.sock")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn event(name: &str, epoch: u64) -> Value {
        json!({
            "protocolVersion": crate::protocol::PROTOCOL_VERSION,
            "kind": "event",
            "name": name,
            "epoch": epoch,
        })
    }

    #[test]
    fn bind_creates_private_socket_and_drop_removes_it() {
        let area = TestDirectory::new();
        let parent = area.path.join("private");
        let path = parent.join("chrome.sock");
        let listener = AppBridgeListener::bind(&path).unwrap();

        assert_eq!(listener.path(), path);
        assert_eq!(fs::symlink_metadata(&parent).unwrap().mode() & 0o777, 0o700);
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), effective_uid());

        drop(listener);
        assert!(!path.exists());
    }

    #[test]
    fn listener_refuses_insecure_parent_and_non_socket_stale_path() {
        let area = TestDirectory::new();
        fs::set_permissions(&area.path, fs::Permissions::from_mode(0o755)).unwrap();
        let error = AppBridgeListener::bind(area.socket_path())
            .err()
            .expect("insecure parent should be rejected")
            .to_string();
        assert!(error.contains("permissions"), "{error}");

        fs::set_permissions(&area.path, fs::Permissions::from_mode(0o700)).unwrap();
        let path = area.socket_path();
        fs::write(&path, b"must not be removed").unwrap();
        let error = AppBridgeListener::bind(&path)
            .err()
            .expect("regular file should be rejected")
            .to_string();
        assert!(error.contains("not a Unix socket"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"must not be removed");
    }

    #[test]
    fn listener_refuses_symlink_parent() {
        let area = TestDirectory::new();
        let real_parent = area.path.join("real");
        let linked_parent = area.path.join("linked");
        fs::create_dir(&real_parent).unwrap();
        fs::set_permissions(&real_parent, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&real_parent, &linked_parent).unwrap();

        let error = AppBridgeListener::bind(linked_parent.join("chrome.sock"))
            .err()
            .expect("symlink parent should be rejected")
            .to_string();
        assert!(error.contains("not a real directory"), "{error}");
    }

    #[test]
    fn bind_reaps_owned_stale_socket_but_refuses_active_listener() {
        let stale_area = TestDirectory::new();
        let stale_path = stale_area.socket_path();
        let stale = UnixListener::bind(&stale_path).unwrap();
        fs::set_permissions(&stale_path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);
        let replacement = AppBridgeListener::bind(&stale_path).unwrap();
        assert_eq!(replacement.path(), stale_path);

        let active_area = TestDirectory::new();
        let active_path = active_area.socket_path();
        let active = UnixListener::bind(&active_path).unwrap();
        fs::set_permissions(&active_path, fs::Permissions::from_mode(0o600)).unwrap();
        let error = AppBridgeListener::bind(&active_path)
            .err()
            .expect("active listener should be preserved")
            .to_string();
        assert!(error.contains("already active"), "{error}");
        assert!(fs::symlink_metadata(&active_path)
            .unwrap()
            .file_type()
            .is_socket());
        drop(active);
    }

    #[test]
    fn listener_drop_does_not_unlink_a_replacement_inode() {
        let area = TestDirectory::new();
        let path = area.socket_path();
        let original = AppBridgeListener::bind(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();

        drop(original);
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_socket());
        drop(replacement);
    }

    #[test]
    fn accepted_connections_are_same_uid_and_preserve_buffered_messages() {
        let area = TestDirectory::new();
        let path = area.socket_path();
        let listener = AppBridgeListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(listener.try_accept().unwrap().is_none());

        let mut client = UnixStream::connect(&path).unwrap();
        let mut connection = listener
            .try_accept()
            .unwrap()
            .expect("connected same-UID client should be accepted");
        assert_eq!(
            peer_uid(connection.reader.get_ref().as_raw_fd()).unwrap(),
            effective_uid()
        );

        let first = event("route_revoked", 2);
        let second = event("pair_expired", 3);
        let mut encoded = encode_ndjson(&first).unwrap();
        encoded.extend_from_slice(&encode_ndjson(&second).unwrap());
        client.write_all(&encoded).unwrap();

        assert!(connection.wait_readable(Duration::from_secs(1)).unwrap());
        assert_eq!(connection.receive().unwrap(), Some(first));
        assert!(
            connection.wait_readable(Duration::ZERO).unwrap(),
            "BufReader-held second line must be reported without another kernel write"
        );
        assert_eq!(connection.receive().unwrap(), Some(second));

        let outbound = event("route_revoked", 4);
        connection.send(&outbound).unwrap();
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(line.trim_end()).unwrap(),
            outbound
        );
    }

    #[test]
    fn connection_receive_rejects_invalid_protocol_and_line_limits() {
        let area = TestDirectory::new();
        let path = area.socket_path();
        let listener = AppBridgeListener::bind(&path).unwrap();
        let mut client = UnixStream::connect(&path).unwrap();
        let mut connection = listener.accept().unwrap();

        client.write_all(b"{}\n").unwrap();
        let error = connection.receive().unwrap_err().to_string();
        assert!(error.contains("protocolVersion"), "{error}");

        let oversized = vec![b'x'; MAX_MESSAGE_BYTES + 2];
        let error = read_line_limited(&mut Cursor::new(oversized))
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds"), "{error}");
    }

    #[test]
    fn paired_connection_preserves_buffered_messages() {
        let (mut peer, stream) = UnixStream::pair().unwrap();
        let mut connection = AppBridgeConnection::new(stream).unwrap();
        let first = event("route_revoked", 2);
        let second = event("pair_expired", 3);
        let mut encoded = encode_ndjson(&first).unwrap();
        encoded.extend_from_slice(&encode_ndjson(&second).unwrap());
        peer.write_all(&encoded).unwrap();

        assert_eq!(connection.receive().unwrap(), Some(first));
        assert!(
            connection.wait_readable(Duration::ZERO).unwrap(),
            "a second frame buffered in userspace must remain immediately readable"
        );
        assert_eq!(connection.receive().unwrap(), Some(second));
    }

    #[test]
    fn partial_line_cannot_pin_the_app_broker_indefinitely() {
        let (mut peer, stream) = UnixStream::pair().unwrap();
        let mut connection = AppBridgeConnection::new(stream).unwrap();
        peer.write_all(b"{").unwrap();

        let started = std::time::Instant::now();
        let error = connection.receive().unwrap_err().to_string();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "partial bridge frame ignored the read deadline"
        );
        assert!(error.contains("receive deadline exceeded"), "{error}");
    }

    #[test]
    fn drip_fed_line_cannot_extend_the_frame_deadline() {
        let (mut peer, stream) = UnixStream::pair().unwrap();
        let mut connection = AppBridgeConnection::new(stream).unwrap();
        let feeder = std::thread::spawn(move || {
            for index in 0..16 {
                let byte = if index == 0 { b'{' } else { b' ' };
                if peer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(75));
            }
        });

        let started = Instant::now();
        let error = connection.receive().unwrap_err().to_string();
        let elapsed = started.elapsed();
        assert!(error.contains("receive deadline exceeded"), "{error}");
        assert!(
            elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(2),
            "drip-fed bridge frame completed after an unexpected {elapsed:?}"
        );

        drop(connection);
        feeder.join().unwrap();
    }

    #[test]
    fn slow_reader_cannot_extend_the_frame_send_deadline() {
        let (mut peer, stream) = UnixStream::pair().unwrap();
        let send_buffer: libc::c_int = 4 * 1024;
        // SAFETY: stream owns a live socket descriptor, and send_buffer is a
        // correctly sized SO_SNDBUF input for the duration of the call.
        let configured = unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&send_buffer as *const libc::c_int).cast(),
                std::mem::size_of_val(&send_buffer) as libc::socklen_t,
            )
        };
        assert_eq!(configured, 0, "failed to reduce test socket send buffer");
        let mut connection = AppBridgeConnection::new(stream).unwrap();
        let slow_reader = std::thread::spawn(move || {
            let mut buffer = [0_u8; 16 * 1024];
            loop {
                std::thread::sleep(Duration::from_millis(75));
                match peer.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let outbound = json!({
            "protocolVersion": crate::protocol::PROTOCOL_VERSION,
            "kind": "event",
            "name": "route_revoked",
            "epoch": 4,
            "padding": "x".repeat(900_000),
        });

        let started = Instant::now();
        let error = connection.send(&outbound).unwrap_err().to_string();
        let elapsed = started.elapsed();
        assert!(error.contains("send deadline exceeded"), "{error}");
        assert!(
            elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(2),
            "slow-reader bridge send completed after an unexpected {elapsed:?}"
        );

        drop(connection);
        slow_reader.join().unwrap();
    }
}
