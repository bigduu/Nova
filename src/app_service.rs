//! App-owned MCP transport for macOS.
//!
//! `nova --connect` is deliberately only a byte proxy.  The MCP server and all
//! of its Accessibility, input, clipboard, window, and capture calls live in
//! the process launched by LaunchServices from `Nova.app`.  That gives TCC one
//! stable, independently grantable application identity instead of attributing
//! permissions to whichever MCP host happened to spawn a stdio child.
//!
//! The transport is also available on Unix headless builds so its security and
//! lifecycle can be exercised in CI.  Automatic application launch remains a
//! macOS-only operation.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub const BUNDLE_ID: &str = "com.zenith.nova";
pub const APP_NAME: &str = "Nova";

/// A duplicate launch is a successful no-op, not a failed service to keep in
/// the menu. The owned listener otherwise runs until cancellation or failure.
#[derive(Debug, PartialEq, Eq)]
pub enum ServiceExit {
    AlreadyRunning,
}

/// True when `path` has the canonical `*.app/Contents/MacOS/<executable>`
/// shape used by LaunchServices.
fn is_bundled_path(path: &Path) -> bool {
    let Some(macos) = path.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(bundle) = contents.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|name| name == "MacOS")
        && contents.file_name().is_some_and(|name| name == "Contents")
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app")
}

/// Whether this executable is the main executable inside an application
/// bundle.  A no-argument LaunchServices invocation uses this to select app
/// service mode; an unbundled binary keeps the historical stdio default.
pub fn is_bundled_executable() -> bool {
    std::env::current_exe()
        .map(|path| is_bundled_path(&path))
        .unwrap_or(false)
}

/// Refuse to host the production macOS service outside Nova.app. Otherwise a
/// tempting manual `nova --app-service` invocation would silently put TCC back
/// on the terminal/Bodhi responsibility chain this transport exists to avoid.
/// Debug builds retain an explicit escape hatch for the transport harness.
#[cfg(target_os = "macos")]
pub fn ensure_service_identity() -> Result<()> {
    if is_bundled_executable()
        || (cfg!(debug_assertions)
            && std::env::var_os("NOVA_APP_ALLOW_UNBUNDLED_SERVICE").as_deref()
                == Some(std::ffi::OsStr::new("1")))
    {
        return Ok(());
    }
    bail!(
        "refusing to host the macOS app service outside Nova.app; launch the app through \
         LaunchServices and use `nova --connect`"
    )
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_service_identity() -> Result<()> {
    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::*;
    use anyhow::Context;
    use std::fs::{File, FileType, OpenOptions};
    use std::io;
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::fs::{
        DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
    };
    #[cfg(target_os = "macos")]
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncWriteExt, BufWriter};
    use tokio::net::{UnixListener, UnixStream};

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
    const CONNECT_RETRY: Duration = Duration::from_millis(50);

    fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and no failure return.
        unsafe { libc::geteuid() }
    }

    /// Default location shared by the app service, the `--connect` proxy, and
    /// the Chrome bridge.  The short `/tmp` path stays below Darwin's Unix
    /// socket path limit; the per-UID parent is verified mode 0700.
    pub fn default_socket_path() -> PathBuf {
        PathBuf::from(format!("/tmp/nova-app-{}/service.sock", effective_uid()))
    }

    /// Resolve the app-service socket.  `NOVA_APP_SOCKET` is intended for
    /// isolated tests and development; it receives the exact same ownership
    /// and permission validation as the default path.
    pub fn socket_path() -> Result<PathBuf> {
        let path = std::env::var_os("NOVA_APP_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(default_socket_path);
        if !path.is_absolute() {
            bail!("NOVA_APP_SOCKET must be an absolute path");
        }
        if path.file_name().is_none() || path.parent().is_none() {
            bail!("invalid app-service socket path: {}", path.display());
        }
        Ok(path)
    }

    fn validate_private_dir(path: &Path) -> Result<()> {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspect app-service directory {}", path.display()))?;
        if !metadata.file_type().is_dir() {
            bail!(
                "refusing app-service path: {} is not a directory",
                path.display()
            );
        }
        if metadata.uid() != effective_uid() {
            bail!(
                "refusing app-service directory {} owned by uid {} (expected {})",
                path.display(),
                metadata.uid(),
                effective_uid()
            );
        }
        let mode = metadata.mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "refusing app-service directory {} with insecure mode {mode:o}; expected 0700",
                path.display()
            );
        }
        Ok(())
    }

    fn ensure_private_dir(path: &Path) -> Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => validate_private_dir(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path
                    .parent()
                    .context("app-service directory has no parent")?;
                if !parent.is_dir() {
                    bail!(
                        "parent of app-service directory does not exist: {}",
                        parent.display()
                    );
                }
                let create_result = std::fs::DirBuilder::new().mode(0o700).create(path);
                if let Err(error) = create_result {
                    // A simultaneous LaunchServices invocation may have made
                    // the directory after our first metadata check. Validate
                    // that winner rather than changing something we did not
                    // create (or failing a harmless startup race).
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        return validate_private_dir(path);
                    }
                    return Err(error).with_context(|| {
                        format!("create private app-service directory {}", path.display())
                    });
                }
                // Do not trust umask alone; assert and normalize the final mode.
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("secure app-service directory {}", path.display()))?;
                validate_private_dir(path)
            }
            Err(error) => Err(error)
                .with_context(|| format!("inspect app-service directory {}", path.display())),
        }
    }

    fn validate_private_socket(
        path: &Path,
        file_type: FileType,
        uid: u32,
        mode: u32,
    ) -> Result<()> {
        if !file_type.is_socket() {
            bail!(
                "refusing app-service path: {} is not a Unix socket",
                path.display()
            );
        }
        if uid != effective_uid() {
            bail!(
                "refusing app-service socket {} owned by uid {uid} (expected {})",
                path.display(),
                effective_uid()
            );
        }
        let permissions = mode & 0o777;
        if permissions & 0o077 != 0 {
            bail!(
                "refusing app-service socket {} with insecure mode {permissions:o}",
                path.display()
            );
        }
        Ok(())
    }

    fn validate_existing_socket(path: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                validate_private_socket(
                    path,
                    metadata.file_type(),
                    metadata.uid(),
                    metadata.mode(),
                )?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("inspect app-service socket {}", path.display()))
            }
        }
    }

    fn open_service_lock(runtime_dir: &Path) -> Result<Option<File>> {
        let lock_path = runtime_dir.join("service.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)
            .with_context(|| format!("open app-service lock {}", lock_path.display()))?;
        let metadata = lock
            .metadata()
            .with_context(|| format!("inspect app-service lock {}", lock_path.display()))?;
        if !metadata.file_type().is_file() || metadata.uid() != effective_uid() {
            bail!(
                "refusing invalid app-service lock file {}",
                lock_path.display()
            );
        }
        lock.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure app-service lock {}", lock_path.display()))?;

        // SAFETY: flock receives an open fd owned by `lock`; the lock remains
        // held until the File is dropped with the service guard.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(lock));
        }
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            Ok(None)
        } else {
            Err(error).context("lock app-service singleton")
        }
    }

    struct ServiceGuard {
        socket: PathBuf,
        _lock: File,
    }

    impl Drop for ServiceGuard {
        fn drop(&mut self) {
            // Only unlink the socket we own.  A replacement service can never
            // acquire the lock until `_lock` drops after this method.
            if let Ok(metadata) = std::fs::symlink_metadata(&self.socket) {
                if metadata.file_type().is_socket() && metadata.uid() == effective_uid() {
                    let _ = std::fs::remove_file(&self.socket);
                }
            }
        }
    }

    fn bind_service(path: &Path) -> Result<Option<(UnixListener, ServiceGuard)>> {
        let runtime_dir = path.parent().context("app-service socket has no parent")?;
        ensure_private_dir(runtime_dir)?;
        let Some(lock) = open_service_lock(runtime_dir)? else {
            return Ok(None);
        };

        if validate_existing_socket(path)? {
            // Holding the singleton lock proves no service from this version is
            // alive.  Removing only a verified same-UID socket is safe.
            std::fs::remove_file(path)
                .with_context(|| format!("remove stale app-service socket {}", path.display()))?;
        }

        // The containing directory is already 0700, so no other UID can reach
        // the socket even during the bind -> chmod interval.
        let listener = std::os::unix::net::UnixListener::bind(path)
            .with_context(|| format!("bind app-service socket {}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure app-service socket {}", path.display()))?;
        listener
            .set_nonblocking(true)
            .context("set app-service listener nonblocking")?;
        let listener = UnixListener::from_std(listener).context("adopt app-service listener")?;
        Ok(Some((
            listener,
            ServiceGuard {
                socket: path.to_path_buf(),
                _lock: lock,
            },
        )))
    }

    #[cfg(target_os = "macos")]
    fn peer_effective_uid(fd: RawFd) -> io::Result<u32> {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: `fd` is a live connected Unix socket and both output
        // pointers reference initialized storage of the required C types.
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } == 0 {
            Ok(uid)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_effective_uid(fd: RawFd) -> io::Result<u32> {
        // Linux exposes the authenticated credentials captured at connect(2)
        // time through SO_PEERCRED.
        // SAFETY: `ucred` is a plain C data structure for which all-zero is a
        // valid initialized value before getsockopt fills every field.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the output buffer and length match `libc::ucred`; `fd` is a
        // live connected Unix socket.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } == 0
        {
            Ok(credentials.uid)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
    fn peer_effective_uid(_fd: RawFd) -> io::Result<u32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "same-UID Unix peer authentication is unsupported on this OS",
        ))
    }

    fn require_same_uid(stream: &UnixStream) -> Result<()> {
        let peer_uid =
            peer_effective_uid(stream.as_raw_fd()).context("authenticate app-service Unix peer")?;
        if peer_uid != effective_uid() {
            bail!(
                "rejected app-service connection from uid {peer_uid}; expected {}",
                effective_uid()
            );
        }
        Ok(())
    }

    /// Run the long-lived, app-owned MCP service.  Each accepted connection is
    /// a separate MCP session, but every handler executes in this process (or
    /// its app-owned capture helper) rather than in the stdio host.
    pub async fn run() -> Result<()> {
        run_with_status(crate::app_status::AppStatus::default())
            .await
            .map(|_| ())
    }

    pub async fn run_with_status(status: crate::app_status::AppStatus) -> Result<ServiceExit> {
        let result = serve(&status).await;
        if result.is_err() {
            status.set_service(crate::app_status::ServiceState::Failed);
        }
        result
    }

    async fn serve(status: &crate::app_status::AppStatus) -> Result<ServiceExit> {
        let socket = socket_path()?;
        let Some((listener, _guard)) = bind_service(&socket)? else {
            tracing::info!(socket = %socket.display(), "Nova app service is already running");
            return Ok(ServiceExit::AlreadyRunning);
        };
        // The Chrome endpoint is owned by the same independent Nova.app
        // process as the MCP service. Bind it once so every MCP session shares
        // one serialized pairing/route authority instead of creating competing
        // per-client brokers.
        let chrome_bridge = nova_chrome_bridge::ChromeBridge::bind_default()
            .context("bind Nova.app Chrome semantic bridge")?;
        status.set_service(crate::app_status::ServiceState::Ready);
        tracing::info!(
            socket = %socket.display(),
            uid = effective_uid(),
            "Nova app-owned MCP service is ready"
        );

        loop {
            let (stream, _) = listener
                .accept()
                .await
                .context("accept app-service client")?;
            if let Err(error) = require_same_uid(&stream) {
                tracing::warn!(%error, "rejected app-service client");
                continue;
            }
            let chrome_bridge = chrome_bridge.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    crate::server::run_unix_stream_with_chrome(stream, chrome_bridge).await
                {
                    tracing::warn!(%error, "app-service MCP session ended with an error");
                }
            });
        }
    }

    fn socket_is_ready(path: &Path) -> Result<bool> {
        let runtime_dir = path.parent().context("app-service socket has no parent")?;
        match std::fs::symlink_metadata(runtime_dir) {
            Ok(_) => validate_private_dir(runtime_dir)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect app-service directory {}", runtime_dir.display())
                })
            }
        }
        validate_existing_socket(path)
    }

    async fn try_connect(path: &Path) -> Result<Option<UnixStream>> {
        if !socket_is_ready(path)? {
            return Ok(None);
        }
        match UnixStream::connect(path).await {
            Ok(stream) => {
                // Close the metadata-check/connect race: authenticate the
                // listening endpoint itself, just as the listener authenticates
                // every connector.
                require_same_uid(&stream).context("authenticate Nova app service")?;
                Ok(Some(stream))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound
                        | io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error)
                .with_context(|| format!("connect to Nova app service at {}", path.display())),
        }
    }

    #[cfg(target_os = "macos")]
    fn launch_with_open(arguments: &[std::ffi::OsString]) -> Result<bool> {
        let status = Command::new("/usr/bin/open")
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("run macOS LaunchServices via /usr/bin/open")?;
        Ok(status.success())
    }

    #[cfg(target_os = "macos")]
    fn bundled_app_path() -> Option<PathBuf> {
        let executable = std::env::current_exe().ok()?;
        if !super::is_bundled_path(&executable) {
            return None;
        }
        executable
            .parent()?
            .parent()?
            .parent()
            .map(Path::to_path_buf)
    }

    #[cfg(target_os = "macos")]
    fn launch_app() -> Result<()> {
        use std::ffi::OsString;

        let explicit_bundle = std::env::var_os("NOVA_APP_BUNDLE").map(PathBuf::from);
        if let Some(bundle) = explicit_bundle.or_else(bundled_app_path) {
            let args = [OsString::from("-gj"), bundle.into_os_string()];
            if launch_with_open(&args)? {
                return Ok(());
            }
        }

        let by_identifier = [
            OsString::from("-gj"),
            OsString::from("-b"),
            OsString::from(super::BUNDLE_ID),
        ];
        if launch_with_open(&by_identifier)? {
            return Ok(());
        }
        let by_name = [
            OsString::from("-gj"),
            OsString::from("-a"),
            OsString::from(super::APP_NAME),
        ];
        if launch_with_open(&by_name)? {
            return Ok(());
        }
        bail!("could not launch Nova.app; install/open Nova.app once, then retry `nova --connect`")
    }

    #[cfg(not(target_os = "macos"))]
    fn launch_app() -> Result<()> {
        bail!("Nova app-service autostart requires macOS LaunchServices")
    }

    async fn connect_or_launch(path: &Path) -> Result<UnixStream> {
        if let Some(stream) = try_connect(path).await? {
            return Ok(stream);
        }

        // An override is process-local and would not be inherited by an app
        // that LaunchServices starts.  Tests/dev using it must start the hidden
        // `--app-service` endpoint explicitly.
        if std::env::var_os("NOVA_APP_SOCKET").is_some() {
            bail!(
                "no Nova app service is listening at overridden socket {}; start `nova --app-service` first",
                path.display()
            );
        }
        launch_app()?;

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            if let Some(stream) = try_connect(path).await? {
                return Ok(stream);
            }
            if Instant::now() >= deadline {
                bail!(
                    "Nova.app launched but its private service did not become ready at {} within {}s",
                    path.display(),
                    CONNECT_TIMEOUT.as_secs()
                );
            }
            tokio::time::sleep(CONNECT_RETRY).await;
        }
    }

    /// Connect stdio to the app-owned MCP socket.  This process never touches
    /// CoreGraphics, Accessibility, or other desktop APIs; it only copies the
    /// MCP NDJSON byte stream in both directions.
    pub async fn connect_stdio() -> Result<()> {
        let path = socket_path()?;
        let stream = connect_or_launch(&path).await?;
        let (mut socket_read, mut socket_write) = stream.into_split();

        let upload = async {
            let mut stdin = tokio::io::stdin();
            tokio::io::copy(&mut stdin, &mut socket_write)
                .await
                .context("forward MCP requests to Nova.app")?;
            socket_write
                .shutdown()
                .await
                .context("close Nova.app request stream")
        };
        let download = async {
            let stdout = tokio::io::stdout();
            let mut stdout = BufWriter::new(stdout);
            tokio::io::copy(&mut socket_read, &mut stdout)
                .await
                .context("forward MCP responses from Nova.app")?;
            stdout.flush().await.context("flush MCP responses")
        };
        tokio::pin!(upload);
        tokio::pin!(download);

        tokio::select! {
            result = &mut download => result,
            result = &mut upload => {
                result?;
                download.await
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicU64, Ordering};

        fn test_dir(label: &str) -> PathBuf {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            std::env::temp_dir().join(format!(
                "nova-app-test-{}-{label}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
        }

        #[test]
        fn recognizes_only_canonical_app_executable_paths() {
            assert!(super::super::is_bundled_path(Path::new(
                "/Applications/Nova.app/Contents/MacOS/nova"
            )));
            assert!(!super::super::is_bundled_path(Path::new(
                "/Applications/Nova.app/MacOS/nova"
            )));
            assert!(!super::super::is_bundled_path(Path::new(
                "/usr/local/bin/nova"
            )));
        }

        #[test]
        fn private_runtime_directory_is_created_and_verified() {
            let directory = test_dir("private");
            ensure_private_dir(&directory).expect("create private directory");
            let metadata = std::fs::symlink_metadata(&directory).unwrap();
            assert!(metadata.file_type().is_dir());
            assert_eq!(metadata.mode() & 0o777, 0o700);
            validate_private_dir(&directory).expect("validate private directory");
            std::fs::remove_dir(&directory).unwrap();
        }

        #[test]
        fn insecure_runtime_directory_is_rejected() {
            let directory = test_dir("insecure");
            std::fs::create_dir(&directory).unwrap();
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
            let error = validate_private_dir(&directory).unwrap_err().to_string();
            assert!(error.contains("insecure mode"), "unexpected error: {error}");
            std::fs::remove_dir(&directory).unwrap();
        }

        #[test]
        fn unix_peer_credentials_match_effective_uid() {
            let (left, _right) = std::os::unix::net::UnixStream::pair().unwrap();
            let uid = peer_effective_uid(left.as_raw_fd()).expect("read peer credentials");
            assert_eq!(uid, effective_uid());
        }
    }
}

#[cfg(unix)]
pub use unix::{connect_stdio, default_socket_path, run, run_with_status, socket_path};

#[cfg(not(unix))]
pub fn default_socket_path() -> PathBuf {
    PathBuf::new()
}

#[cfg(not(unix))]
pub fn socket_path() -> Result<PathBuf> {
    bail!("Nova app service requires a Unix-domain socket")
}

#[cfg(not(unix))]
pub async fn run() -> Result<()> {
    bail!("Nova app service is available on macOS only")
}

#[cfg(not(unix))]
pub async fn connect_stdio() -> Result<()> {
    bail!("`nova --connect` is available on macOS only")
}
