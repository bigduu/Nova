//! Windows local transport; pairing and receipt authority stay in app.rs.
use crate::framing::{encode_native, encode_ndjson, NativeDecoder, NdjsonDecoder};
use crate::protocol::{
    host_hello, redacted_diagnostic, validate_extension_origin, validate_message,
};
use ::windows::core::{PCWSTR, PWSTR};
use ::windows::Win32::Foundation::*;
use ::windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use ::windows::Win32::Security::*;
use ::windows::Win32::Storage::FileSystem::*;
use ::windows::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use ::windows::Win32::System::Pipes::*;
use ::windows::Win32::System::Threading::*;
use ::windows::Win32::System::IO::*;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::env;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const FRAME_TIMEOUT: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(20);
const PIPE_PREFIX: &str = r"\\.\pipe\nova-chrome-";
// SE_GROUP_LOGON_ID from winnt.h.
const LOGON_GROUP: u32 = 0xc0000000;

fn check_partial_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        bail!("Chrome incomplete-frame deadline exceeded");
    }
    Ok(())
}

fn update_partial_deadline(deadline: &mut Option<Instant>, partial: bool, completed: bool) {
    if !partial {
        *deadline = None;
    } else if deadline.is_none() || completed {
        // A decoded message ends the previous frame; any retained tail starts a
        // new one. More bytes for the same incomplete frame never renew its time.
        *deadline = Some(Instant::now() + FRAME_TIMEOUT);
    }
}

struct Handle(HANDLE);
// SAFETY: owned kernel handles may be used across threads; callers coordinate I/O.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns one valid, non-pseudo kernel handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct LocalMemory(*mut std::ffi::c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: the security conversion APIs allocate with LocalAlloc.
        unsafe {
            let _ = LocalFree(HLOCAL(self.0));
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn sid_string(sid: PSID) -> Result<String> {
    let mut output = PWSTR::null();
    // SAFETY: sid comes from a live token information buffer.
    unsafe {
        ConvertSidToStringSidW(sid, &mut output)?;
        let _allocation = LocalMemory(output.0.cast());
        output.to_string().map_err(Into::into)
    }
}

fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    let mut length = 0;
    // SAFETY: the first call queries size; the second uses an aligned live buffer.
    unsafe {
        let _ = GetTokenInformation(token, class, None, 0, &mut length);
        if length == 0 {
            bail!("cannot inspect Windows peer token");
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        GetTokenInformation(
            token,
            class,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )?;
        Ok(buffer)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    user: String,
    logon: String,
    session: u32,
}

impl Identity {
    fn for_process(process: HANDLE) -> Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: process is a live handle and token is writable output storage.
        unsafe {
            OpenProcessToken(process, TOKEN_QUERY, &mut token)?;
        }
        let token = Handle(token);
        let user = token_info(token.0, TokenUser)?;
        let groups = token_info(token.0, TokenGroups)?;
        let session = token_info(token.0, TokenSessionId)?;
        // SAFETY: these buffers were populated by GetTokenInformation for their
        // respective classes, are usize-aligned, and remain alive during conversion.
        unsafe {
            let user = &*user.as_ptr().cast::<TOKEN_USER>();
            let groups = &*groups.as_ptr().cast::<TOKEN_GROUPS>();
            let entries =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            let logon = entries.iter().find(|group| group.Attributes & LOGON_GROUP == LOGON_GROUP)
                .context("Windows Chrome bridge requires a logon-session token; run managed Nova and Chrome in the same user session")?;
            Ok(Self {
                user: sid_string(user.User.Sid)?,
                logon: sid_string(logon.Sid)?,
                session: *session.as_ptr().cast::<u32>(),
            })
        }
    }

    fn current() -> Result<Self> {
        // SAFETY: GetCurrentProcess returns a valid borrowed pseudo-handle.
        Self::for_process(unsafe { GetCurrentProcess() })
    }
}

pub fn default_pipe_path() -> Result<PathBuf> {
    let identity = Identity::current()?;
    let digest = Sha256::digest(format!(
        "{}:{}:{}",
        identity.user, identity.logon, identity.session
    ));
    Ok(PathBuf::from(format!("{PIPE_PREFIX}{:x}", digest)))
}

fn expected_extension_id() -> Result<String> {
    let id = env::var("NOVA_CHROME_EXTENSION_ID")
        .context("set NOVA_CHROME_EXTENSION_ID to the exact 32-character Chrome extension ID")?;
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        bail!("NOVA_CHROME_EXTENSION_ID must contain exactly 32 characters from a through p");
    }
    Ok(id)
}

pub fn managed_chrome_configured() -> Result<bool> {
    if env::var_os("NOVA_CHROME_EXTENSION_ID").is_some() {
        expected_extension_id()?;
        Ok(true)
    } else if env::var_os("NOVA_CHROME_PIPE").is_some() {
        bail!("NOVA_CHROME_PIPE requires the exact NOVA_CHROME_EXTENSION_ID; configure both for managed nova mcp");
    } else {
        Ok(false)
    }
}

pub(crate) fn configured_pipe_path() -> Result<PathBuf> {
    expected_extension_id()?;
    let path = match env::var_os("NOVA_CHROME_PIPE") {
        Some(path) => PathBuf::from(path),
        None => default_pipe_path()?,
    };
    pipe_name(&path)?;
    Ok(path)
}

fn pipe_name(path: &Path) -> Result<Vec<u16>> {
    let name = path.to_str().context("Chrome pipe name must be Unicode")?;
    let suffix = name
        .strip_prefix(PIPE_PREFIX)
        .context("Chrome endpoint must be a local \\\\.\\pipe\\nova-chrome- name")?;
    if suffix.is_empty()
        || name.len() > 240
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("Chrome pipe name must have a non-empty alphanumeric or hyphen suffix");
    }
    Ok(wide(name))
}

struct Pipe {
    handle: Handle,
    identity: Identity,
    connected: AtomicBool,
}

fn verify_peer(pipe: &Pipe, server_side: bool) -> Result<()> {
    let mut pid = 0;
    // SAFETY: pipe is connected, pid is output storage, and the process handle is
    // query-only. Identity is taken from the kernel, never from a JSON pid field.
    unsafe {
        if server_side {
            GetNamedPipeClientProcessId(pipe.handle.0, &mut pid)?;
        } else {
            GetNamedPipeServerProcessId(pipe.handle.0, &mut pid)?;
        }
        let process = Handle(
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .context("cannot inspect Chrome bridge peer process")?,
        );
        if Identity::for_process(process.0)? != pipe.identity {
            bail!("Chrome bridge peer is not the same user and logon session");
        }
    }
    Ok(())
}

/// One stable allocation per outstanding kernel operation. Drop cancels and
/// drains before either the OVERLAPPED, its event, or its borrowed buffer is freed.
struct Operation {
    pipe: Arc<Pipe>,
    event: Handle,
    overlapped: Box<OVERLAPPED>,
    pending: bool,
}
// SAFETY: the operation is exclusively owned and all kernel-referenced storage
// is boxed. Moving it between threads does not move that storage.
unsafe impl Send for Operation {}

impl Operation {
    fn new(pipe: Arc<Pipe>) -> Result<Self> {
        // SAFETY: unnamed, non-inheritable, manual-reset event.
        let event = Handle(unsafe { CreateEventW(None, true, false, PCWSTR::null())? });
        let overlapped = Box::new(OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        });
        Ok(Self {
            pipe,
            event,
            overlapped,
            pending: false,
        })
    }

    fn started(&mut self, result: ::windows::core::Result<()>) -> Result<()> {
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
                self.pending = true;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ready(&self, timeout: Duration) -> Result<bool> {
        if !self.pending {
            return Ok(true);
        }
        let millis = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: event remains live for this pending operation.
        match unsafe { WaitForSingleObject(self.event.0, millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => bail!("wait for Chrome pipe operation failed"),
        }
    }

    fn complete(&mut self, deadline: Instant) -> Result<u32> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !self.ready(remaining)? {
            bail!("Chrome bridge frame deadline exceeded");
        }
        let mut count = 0;
        // SAFETY: the operation and its buffer stay alive until this completion.
        let result =
            unsafe { GetOverlappedResult(self.pipe.handle.0, &*self.overlapped, &mut count, true) };
        self.pending = false;
        result?;
        Ok(count)
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if self.pending {
            let mut count = 0;
            // SAFETY: cancellation is only a request. Waiting for completion here
            // preserves OVERLAPPED/buffer lifetime even if cancellation races success.
            unsafe {
                let _ = CancelIoEx(self.pipe.handle.0, Some(&*self.overlapped));
                let _ =
                    GetOverlappedResult(self.pipe.handle.0, &*self.overlapped, &mut count, true);
            }
        }
    }
}

pub struct AppBridgeListener {
    pipe: Arc<Pipe>,
    path: PathBuf,
    connecting: Mutex<Option<Operation>>,
    nonblocking: AtomicBool,
}

impl AppBridgeListener {
    pub fn bind(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let name = pipe_name(path)?;
        let identity = Identity::current()?;
        // A user-wide allow ACE would bypass the session restriction. Explicit
        // owner plus the logon SID alone grants access only to this logon.
        let sddl = wide(&format!(
            "O:{}D:P(A;;GA;;;{})",
            identity.user, identity.logon
        ));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the converted descriptor and name remain live through creation.
        let handle = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )?;
            let _allocation = LocalMemory(descriptor.0);
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let handle = CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                16 * 1024,
                16 * 1024,
                500,
                Some(&attributes),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(anyhow!(::windows::core::Error::from_win32()).context(
                    "cannot own the Chrome pipe: another broker owns this endpoint or Windows denied access; do not start a second configured nova mcp",
                ));
            }
            handle
        };
        Ok(Self {
            pipe: Arc::new(Pipe {
                handle: Handle(handle),
                identity,
                connected: AtomicBool::new(false),
            }),
            path: path.to_owned(),
            connecting: Mutex::new(None),
            nonblocking: AtomicBool::new(false),
        })
    }

    pub fn set_nonblocking(&self, value: bool) -> Result<()> {
        // Overlapped I/O supplies non-blocking accept, never legacy PIPE_NOWAIT.
        self.nonblocking.store(value, Ordering::Relaxed);
        Ok(())
    }

    pub fn accept(&self) -> Result<AppBridgeConnection> {
        loop {
            if let Some(connection) = self.try_accept()? {
                return Ok(connection);
            }
            if self.nonblocking.load(Ordering::Relaxed) {
                bail!("Chrome pipe accept would block");
            }
            std::thread::sleep(POLL);
        }
    }

    pub fn try_accept(&self) -> Result<Option<AppBridgeConnection>> {
        if self.pipe.connected.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut connecting = self
            .connecting
            .lock()
            .map_err(|_| anyhow!("Chrome pipe accept lock poisoned"))?;
        if self.pipe.connected.load(Ordering::Acquire) {
            return Ok(None);
        }
        if connecting.is_none() {
            let mut operation = Operation::new(self.pipe.clone())?;
            // SAFETY: this stable OVERLAPPED belongs to the listener until complete.
            let result =
                unsafe { ConnectNamedPipe(self.pipe.handle.0, Some(&mut *operation.overlapped)) };
            match result {
                Err(error) if error.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
                result => {
                    if let Err(error) = operation.started(result) {
                        // A client can close between CreateFile and ConnectNamedPipe.
                        // Reset that disconnected instance without surrendering ownership.
                        unsafe {
                            let _ = DisconnectNamedPipe(self.pipe.handle.0);
                        }
                        return Err(error);
                    }
                    *connecting = Some(operation);
                }
            }
        }
        if let Some(operation) = connecting.as_mut() {
            if !operation.ready(Duration::ZERO)? {
                return Ok(None);
            }
            let completed = operation.complete(Instant::now() + FRAME_TIMEOUT);
            *connecting = None;
            if let Err(error) = completed {
                // SAFETY: the completed/canceled connect operation has been drained.
                unsafe {
                    let _ = DisconnectNamedPipe(self.pipe.handle.0);
                }
                return Err(error);
            }
        }
        self.pipe.connected.store(true, Ordering::Release);
        if let Err(error) = verify_peer(&self.pipe, true) {
            // SAFETY: no I/O is pending on this accepted instance.
            unsafe {
                let _ = DisconnectNamedPipe(self.pipe.handle.0);
            }
            self.pipe.connected.store(false, Ordering::Release);
            return Err(error);
        }
        match AppBridgeConnection::new(self.pipe.clone(), true) {
            Ok(connection) => Ok(Some(connection)),
            Err(error) => {
                // SAFETY: no accepted connection or pending I/O was exposed.
                unsafe {
                    let _ = DisconnectNamedPipe(self.pipe.handle.0);
                }
                self.pipe.connected.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct PendingRead {
    operation: Operation,
    buffer: Box<[u8; 16 * 1024]>,
}

pub struct AppBridgeConnection {
    pipe: Arc<Pipe>,
    server_side: bool,
    read: Option<PendingRead>,
    decoder: NdjsonDecoder,
    partial_deadline: Option<Instant>,
    messages: VecDeque<Value>,
    expected_id: Option<String>,
    eof: bool,
}

impl AppBridgeConnection {
    fn new(pipe: Arc<Pipe>, server_side: bool) -> Result<Self> {
        let expected_id = if env::var_os("NOVA_CHROME_EXTENSION_ID").is_some() {
            Some(expected_extension_id()?)
        } else {
            None
        };
        Ok(Self {
            pipe,
            server_side,
            read: None,
            decoder: NdjsonDecoder::default(),
            partial_deadline: None,
            messages: VecDeque::new(),
            expected_id,
            eof: false,
        })
    }

    pub fn connect(path: impl AsRef<Path>) -> Result<Self> {
        let name = pipe_name(path.as_ref())?;
        let deadline = Instant::now() + FRAME_TIMEOUT;
        let handle = loop {
            // Readiness does not reserve an instance; another client may win the
            // next open. Every BUSY retry shares this one original deadline.
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("Chrome pipe connection deadline exceeded; the current broker has not resumed accepting clients");
            }
            // SAFETY: local name, no inherited handle, identity-only client SQOS.
            match unsafe {
                CreateFileW(PCWSTR(name.as_ptr()), GENERIC_READ.0 | GENERIC_WRITE.0, FILE_SHARE_MODE(0), None,
                    OPEN_EXISTING, FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, None)
            } {
                Ok(handle) => break handle,
                Err(error) if error.code() == ERROR_PIPE_BUSY.to_hresult() => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        bail!("Chrome pipe connection deadline exceeded; the current broker has not resumed accepting clients");
                    }
                    // Only BUSY can be the ordinary disconnect/re-arm interval.
                    // SAFETY: name stays live; a nonzero bounded wait avoids the
                    // zero/default-wait sentinel and never changes pipe ownership.
                    match unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), remaining.as_millis().max(1) as u32) }.ok() {
                        Err(error) if error.code() == ERROR_SEM_TIMEOUT.to_hresult() => {
                            return Err(anyhow!(error).context("Chrome pipe connection deadline exceeded; the current broker has not resumed accepting clients"));
                        }
                        result => result.context("waiting for the busy Chrome pipe failed")?,
                    }
                }
                Err(error) => return Err(anyhow!(error).context("Chrome pipe is unavailable; start the configured managed nova mcp in the same user session")),
            }
        };
        let pipe = Arc::new(Pipe {
            handle: Handle(handle),
            identity: Identity::current()?,
            connected: AtomicBool::new(true),
        });
        verify_peer(&pipe, false)?;
        Self::new(pipe, false)
    }

    fn begin_read(&mut self) -> Result<()> {
        if self.read.is_some() {
            return Ok(());
        }
        let mut read = PendingRead {
            operation: Operation::new(self.pipe.clone())?,
            buffer: Box::new([0; 16 * 1024]),
        };
        // SAFETY: both heap allocations stay live in self.read until completion.
        let result = unsafe {
            ReadFile(
                self.pipe.handle.0,
                Some(&mut read.buffer[..]),
                None,
                Some(&mut *read.operation.overlapped),
            )
        };
        match result {
            Err(error) if error.code() == ERROR_BROKEN_PIPE.to_hresult() => {
                self.decoder.finish()?;
                self.eof = true;
                return Ok(());
            }
            result => read.operation.started(result)?,
        }
        self.read = Some(read);
        Ok(())
    }

    pub fn wait_readable(&mut self, timeout: Duration) -> Result<bool> {
        check_partial_deadline(self.partial_deadline)?;
        if !self.messages.is_empty() || self.eof || self.partial_deadline.is_some() {
            return Ok(true);
        }
        self.begin_read()?;
        match self.read.as_ref() {
            Some(read) => read.operation.ready(timeout),
            None => Ok(true),
        }
    }

    pub fn receive(&mut self) -> Result<Option<Value>> {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        loop {
            check_partial_deadline(self.partial_deadline)?;
            if let Some(value) = self.messages.pop_front() {
                self.validate(&value)?;
                return Ok(Some(value));
            }
            if self.eof {
                return Ok(None);
            }
            self.begin_read()?;
            let Some(mut read) = self.read.take() else {
                continue;
            };
            let frame_deadline = self.partial_deadline.unwrap_or(deadline).min(deadline);
            let count = match read.operation.complete(frame_deadline) {
                Ok(count) => count as usize,
                Err(error)
                    if error
                        .downcast_ref::<::windows::core::Error>()
                        .is_some_and(|error| error.code() == ERROR_BROKEN_PIPE.to_hresult()) =>
                {
                    self.decoder.finish()?;
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            if count == 0 {
                self.decoder.finish()?;
                return Ok(None);
            }
            let messages = self.decoder.push(&read.buffer[..count])?;
            update_partial_deadline(
                &mut self.partial_deadline,
                self.decoder.finish().is_err(),
                !messages.is_empty(),
            );
            self.messages.extend(messages);
        }
    }

    fn validate(&self, value: &Value) -> Result<()> {
        validate_message(value)?;
        if matches!(
            value.get("kind").and_then(Value::as_str),
            Some("hello" | "host_hello")
        ) {
            if let Some(expected) = &self.expected_id {
                if value.get("extensionId").and_then(Value::as_str) != Some(expected.as_str()) {
                    bail!(
                        "Chrome handshake identity differs from the exact configured extension ID"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn send(&mut self, value: &Value) -> Result<()> {
        self.validate(value)?;
        self.write_bytes(&encode_ndjson(value)?)
    }

    fn write_bytes(&mut self, mut bytes: &[u8]) -> Result<()> {
        let deadline = Instant::now() + FRAME_TIMEOUT;
        while !bytes.is_empty() {
            let mut operation = Operation::new(self.pipe.clone())?;
            // SAFETY: bytes remain borrowed until operation completes or Drop
            // cancels and drains it, including all error and timeout paths.
            let result = unsafe {
                WriteFile(
                    self.pipe.handle.0,
                    Some(bytes),
                    None,
                    Some(&mut *operation.overlapped),
                )
            };
            operation.started(result)?;
            let count = operation.complete(deadline)? as usize;
            if count == 0 {
                bail!("Chrome pipe closed during send");
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
}

impl Drop for AppBridgeConnection {
    fn drop(&mut self) {
        // Drain any speculative read before disconnecting; the listener retains
        // the SAME kernel pipe handle throughout reconnect, without a bind gap.
        drop(self.read.take());
        if self.server_side {
            // SAFETY: this connection has no remaining outstanding operations.
            unsafe {
                let _ = DisconnectNamedPipe(self.pipe.handle.0);
            }
            self.pipe.connected.store(false, Ordering::Release);
        }
    }
}

struct NativeWriter {
    sender: Option<mpsc::SyncSender<Vec<u8>>>,
    completed: mpsc::Receiver<Result<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NativeWriter {
    fn new() -> Result<Self> {
        let (sender, frames) = mpsc::sync_channel::<Vec<u8>>(1);
        let (reply, completed) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("nova-chrome-stdout".into())
            .spawn(move || {
                while let Ok(frame) = frames.recv() {
                    let result = (|| -> Result<()> {
                        // SAFETY: stdio is borrowed and WriteFile preserves binary bytes.
                        let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE)? };
                        let mut bytes = frame.as_slice();
                        while !bytes.is_empty() {
                            let mut written = 0;
                            unsafe {
                                WriteFile(stdout, Some(bytes), Some(&mut written), None)?;
                            }
                            if written == 0 {
                                bail!("Chrome stdout closed");
                            }
                            bytes = &bytes[written as usize..];
                        }
                        Ok(())
                    })();
                    let failed = result.is_err();
                    if reply.send(result).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            completed,
            thread: Some(thread),
        })
    }

    fn send(&self, value: &Value) -> Result<()> {
        self.sender
            .as_ref()
            .context("Chrome stdout stopped")?
            .send(encode_native(value)?)
            .map_err(|_| anyhow!("Chrome stdout stopped"))?;
        self.completed
            .recv_timeout(FRAME_TIMEOUT)
            .context("Chrome native-message send deadline exceeded")?
    }
}

impl Drop for NativeWriter {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            // Anonymous Chrome stdout is synchronous. Retry cancellation until
            // this one worker ends, covering cancellation just before WriteFile.
            // Its frame buffer remains on the worker until the write completes.
            while !thread.is_finished() {
                // SAFETY: the JoinHandle owns this live thread handle.
                unsafe {
                    let _ = CancelSynchronousIo(HANDLE(thread.as_raw_handle()));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = thread.join();
        }
    }
}

pub(crate) fn run_host() -> Result<()> {
    expected_extension_id()?;
    let origin = env::args()
        .nth(1)
        .context("Chrome did not provide the extension origin")?;
    let extension_id = validate_extension_origin(&origin)?;
    let mut app = AppBridgeConnection::connect(configured_pipe_path()?)?;
    app.send(&host_hello(&extension_id)?)?;
    // SAFETY: borrowed stdin is the binary anonymous pipe supplied by Chrome.
    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE)? };
    let stdout = NativeWriter::new()?;
    let mut decoder = NativeDecoder::default();
    let mut partial_deadline = None;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        check_partial_deadline(partial_deadline)?;
        let mut available = 0;
        // Peek avoids a blocked stdin reader keeping the host alive after broker
        // loss. We read at most available bytes with no competing stdin reader.
        let peek = unsafe { PeekNamedPipe(stdin, None, 0, None, Some(&mut available), None) };
        match peek {
            Err(error) if error.code() == ERROR_BROKEN_PIPE.to_hresult() => {
                decoder.finish()?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
            Ok(()) => {}
        }
        if available > 0 {
            let count = (available as usize).min(buffer.len());
            let mut read = 0;
            // SAFETY: buffer is writable, count is available, and stdio is raw binary.
            unsafe {
                ReadFile(stdin, Some(&mut buffer[..count]), Some(&mut read), None)?;
            }
            let messages = decoder.push(&buffer[..read as usize])?;
            update_partial_deadline(
                &mut partial_deadline,
                decoder.finish().is_err(),
                !messages.is_empty(),
            );
            for message in messages {
                validate_message(&message)?;
                eprintln!(
                    "nova-chrome-host extension_to_app: {}",
                    redacted_diagnostic(&message)?
                );
                app.send(&message)?;
            }
        }
        if app.wait_readable(POLL)? {
            let Some(message) = app.receive()? else {
                return Ok(());
            };
            eprintln!(
                "nova-chrome-host app_to_extension: {}",
                redacted_diagnostic(&message)?
            );
            stdout.send(&message)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    fn endpoint() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        PathBuf::from(format!(
            "{PIPE_PREFIX}test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn connected() -> (AppBridgeListener, AppBridgeConnection, AppBridgeConnection) {
        let listener = AppBridgeListener::bind(endpoint()).unwrap();
        let client = AppBridgeConnection::connect(listener.path()).unwrap();
        let server = listener.accept().unwrap();
        (listener, client, server)
    }

    #[test]
    fn windows_pipe_keeps_first_owner_through_disconnect_and_reconnect() {
        let (listener, client, server) = connected();
        assert!(AppBridgeListener::bind(listener.path()).is_err());
        drop(server);
        drop(client);
        assert!(AppBridgeListener::bind(listener.path()).is_err());
        assert!(listener.try_accept().unwrap().is_none());
        let early = AppBridgeConnection::connect(listener.path()).unwrap();
        drop(early);
        // Closing before accept must reset this instance without releasing it.
        drop(listener.try_accept());
        assert!(listener.try_accept().unwrap().is_none());
        let _client = AppBridgeConnection::connect(listener.path()).unwrap();
        let server = listener.accept().unwrap();
        assert!(AppBridgeListener::bind(listener.path()).is_err());
        drop(server);
        drop(_client);
        let path = listener.path().to_owned();
        drop(listener);
        assert!(AppBridgeListener::bind(path).is_ok());
    }

    #[test]
    fn windows_pipe_busy_connect_deadline_preserves_the_occupied_or_disconnected_owner() {
        for disconnect in [false, true] {
            let (listener, client, server) = connected();
            if disconnect {
                drop(server);
                drop(client);
            }
            let started = Instant::now();
            let error = AppBridgeConnection::connect(listener.path())
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("connection deadline"), "{error}");
            assert!(started.elapsed() >= Duration::from_millis(300));
            assert!(started.elapsed() < Duration::from_secs(2));
            assert!(AppBridgeListener::bind(listener.path()).is_err());
        }
    }

    #[test]
    fn windows_pipe_partial_and_drip_fed_frames_have_one_absolute_deadline() {
        for drip in [false, true] {
            let (_listener, mut client, mut server) = connected();
            client.write_bytes(b"{").unwrap();
            let feeder = std::thread::spawn(move || {
                if drip {
                    for _ in 0..16 {
                        std::thread::sleep(Duration::from_millis(75));
                        if client.write_bytes(b" ").is_err() {
                            break;
                        }
                    }
                } else {
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
            let started = Instant::now();
            assert!(server.wait_readable(POLL).unwrap());
            let error = server.receive().unwrap_err().to_string();
            assert!(error.contains("deadline"), "{error}");
            assert!(started.elapsed() >= Duration::from_millis(300));
            assert!(started.elapsed() < Duration::from_secs(2));
            drop(server);
            feeder.join().unwrap();
        }
    }

    #[test]
    fn windows_pipe_readiness_expires_a_partial_tail_after_returning_a_complete_frame() {
        let (_listener, mut client, mut server) = connected();
        let hello = host_hello("abcdefghijklmnopabcdefghijklmnop").unwrap();
        let mut bytes = encode_ndjson(&hello).unwrap();
        bytes.push(b'{');
        client.write_bytes(&bytes).unwrap();
        assert!(server.wait_readable(POLL).unwrap());
        assert_eq!(server.receive().unwrap(), Some(hello));
        assert!(server.wait_readable(Duration::ZERO).unwrap());
        // No subsequent bytes arrive. The production polling gate must still
        // reject the retained tail using the deadline recorded before returning.
        std::thread::sleep(FRAME_TIMEOUT + POLL);
        let error = server.wait_readable(POLL).unwrap_err().to_string();
        assert!(error.contains("deadline"), "{error}");
    }

    #[test]
    fn windows_pipe_unread_write_cancels_and_drains_then_reconnects() {
        let (listener, client, mut server) = connected();
        let message = json!({"protocolVersion":1, "kind":"event", "name":"route_revoked", "epoch":1, "padding":"x".repeat(900_000)});
        let started = Instant::now();
        let error = server.send(&message).unwrap_err().to_string();
        assert!(error.contains("deadline"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(server);
        drop(client);
        assert!(listener.try_accept().unwrap().is_none());
        let mut client = AppBridgeConnection::connect(listener.path()).unwrap();
        let mut server = listener.accept().unwrap();
        client
            .send(&json!({"protocolVersion":1, "kind":"event", "name":"route_revoked", "epoch":2}))
            .unwrap();
        assert_eq!(server.receive().unwrap().unwrap()["epoch"], 2);
        assert!(server.wait_readable(Duration::ZERO).is_ok());
        // Drop must also cancel a speculative read that has no bytes available.
        drop(server);
        drop(client);
        assert!(AppBridgeListener::bind(listener.path()).is_err());
    }

    #[test]
    fn windows_production_acl_denies_an_actually_impersonated_restricted_token() {
        let listener = AppBridgeListener::bind(endpoint()).unwrap();
        let path = listener.path().to_owned();
        std::thread::spawn(move || {
            // SAFETY: token handles are owned, buffers remain live, and the
            // restricted token belongs to our own identity. No account/policy changes.
            unsafe {
                let mut token = HANDLE::default();
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    &mut token,
                )
                .unwrap();
                let token = Handle(token);
                let mut sid = [0usize; 9];
                let mut size = std::mem::size_of_val(&sid) as u32;
                let sid_pointer = PSID(sid.as_mut_ptr().cast());
                CreateWellKnownSid(WinNullSid, None, sid_pointer, &mut size).unwrap();
                let restricting = [SID_AND_ATTRIBUTES {
                    Sid: sid_pointer,
                    Attributes: 0,
                }];
                let mut restricted = HANDLE::default();
                CreateRestrictedToken(
                    token.0,
                    DISABLE_MAX_PRIVILEGE,
                    None,
                    None,
                    Some(&restricting),
                    &mut restricted,
                )
                .unwrap();
                let restricted = Handle(restricted);
                ImpersonateLoggedOnUser(restricted.0)
                    .expect("the denial test must actually impersonate");
                struct Revert;
                impl Drop for Revert {
                    fn drop(&mut self) {
                        // SAFETY: this guard is on the successfully impersonated thread.
                        unsafe {
                            RevertToSelf().expect("revert test impersonation");
                        }
                    }
                }
                let _revert = Revert;
                let mut active = HANDLE::default();
                OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut active).unwrap();
                let active = Handle(active);
                assert!(
                    IsTokenRestricted(active.0).is_ok(),
                    "a normal token would invalidate this test"
                );
                let name = pipe_name(&path).unwrap();
                let denied = CreateFileW(
                    PCWSTR(name.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                );
                if let Ok(handle) = denied {
                    let _ = CloseHandle(handle);
                    panic!("production pipe accepted restricted token");
                }
                assert_eq!(denied.unwrap_err().code(), ERROR_ACCESS_DENIED.to_hresult());
            }
        })
        .join()
        .unwrap();
        assert!(listener.try_accept().unwrap().is_none());
        let _normal_peer = AppBridgeConnection::connect(listener.path()).unwrap();
        assert!(
            listener.accept().is_ok(),
            "the same production ACL must permit the ordinary owner"
        );
    }

    #[test]
    fn windows_pipe_rejects_remote_and_unbounded_endpoint_names() {
        for name in [
            r"\\remote\pipe\nova-chrome-test",
            r"\\.\pipe\another-test",
            r"\\.\pipe\nova-chrome-",
            r"\\.\pipe\nova-chrome-a\b",
        ] {
            assert!(pipe_name(Path::new(name)).is_err());
        }
    }
}
