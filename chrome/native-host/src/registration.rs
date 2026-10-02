//! One current-user Google Chrome registration. No broker or desktop lifecycle.
use crate::windows::{default_pipe_path, pipe_name, validate_extension_id};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

const HOST_NAME: &str = "com.zenith.nova.chrome";
const BINARY_NAME: &str = "nova-chrome-host.exe";
const MANIFEST_NAME: &str = "com.zenith.nova.chrome.json";
pub(crate) const CONFIG_NAME: &str = "nova-chrome-host.json";
const CHROME_LEAF: &str = r"Software\Google\Chrome\NativeMessagingHosts\com.zenith.nova.chrome";
const JSON_LIMIT: u64 = 4096;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

// Check existing ancestors as well: a redirected Nova directory must not turn
// a fixed-name operation into a write or deletion outside this installation.
fn metadata(path: &Path) -> Result<Option<Metadata>> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(info) if info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 => bail!(
                "refusing reparse target {}; restore the original owned path",
                ancestor.display()
            ),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", ancestor.display()))
            }
        }
    }
    match fs::symlink_metadata(path) {
        Ok(info) => Ok(Some(info)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let info = metadata(path)?.with_context(|| format!("missing {}", path.display()))?;
    if !info.is_file() || info.len() > JSON_LIMIT {
        bail!(
            "{} must be a regular JSON file of at most {JSON_LIMIT} bytes",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(JSON_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > JSON_LIMIT {
        bail!("{} exceeds the configuration size limit", path.display());
    }
    serde_json::from_slice(&bytes).with_context(|| format!("invalid JSON in {}", path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostConfig {
    pub(crate) extension_id: String,
    pipe: Option<PathBuf>,
    binary_sha256: String,
}

impl HostConfig {
    fn new(extension_id: &str, pipe: Option<&Path>, binary_sha256: String) -> Result<Self> {
        validate_extension_id(extension_id)?;
        if let Some(pipe) = pipe {
            pipe_name(pipe)?;
        }
        Ok(Self {
            extension_id: extension_id.into(),
            pipe: pipe.map(Path::to_path_buf),
            binary_sha256,
        })
    }

    fn json(&self) -> Value {
        let mut value = json!({"schemaVersion":1,"owner":HOST_NAME,
            "extensionId":self.extension_id,"binarySha256":self.binary_sha256});
        if let Some(pipe) = &self.pipe {
            value["pipe"] = json!(pipe);
        }
        value
    }

    pub(crate) fn resolved_pipe(&self) -> Result<PathBuf> {
        let pipe = match &self.pipe {
            Some(pipe) => pipe.clone(),
            None => default_pipe_path()?,
        };
        pipe_name(&pipe)?;
        Ok(pipe)
    }
}

pub(crate) fn read_host_config(path: &Path) -> Result<HostConfig> {
    let value = read_json(path)?;
    let object = value
        .as_object()
        .context("native host configuration must be an object")?;
    if value["schemaVersion"] != 1
        || value["owner"] != HOST_NAME
        || object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "schemaVersion" | "owner" | "extensionId" | "binarySha256" | "pipe"
            )
        })
    {
        bail!(
            "unsupported native host configuration; uninstall the owned installation and reinstall"
        );
    }
    let id = value["extensionId"]
        .as_str()
        .context("configuration requires extensionId")?;
    let hash = value["binarySha256"]
        .as_str()
        .context("configuration requires binarySha256")?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid native host binarySha256 ownership record");
    }
    let pipe = object
        .get("pipe")
        .map(|pipe| {
            pipe.as_str()
                .map(PathBuf::from)
                .context("configuration pipe must be a Unicode string")
        })
        .transpose()?;
    HostConfig::new(id, pipe.as_deref(), hash.into())
}

fn digest(path: &Path) -> Result<String> {
    let info = metadata(path)?.context("host binary is missing")?;
    if !info.is_file() || info.len() == 0 {
        bail!("{} must be a non-empty regular host binary", path.display());
    }
    let mut input = File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the successfully opened/created registry handle.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

impl Key {
    fn open(path: &str, access: REG_SAM_FLAGS, view: REG_SAM_FLAGS) -> Result<Option<Self>> {
        let path = wide(path);
        let mut key = HKEY::default();
        // SAFETY: HKCU is borrowed, path is terminated Unicode, key is output storage.
        let result = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                0,
                access | view,
                &mut key,
            )
        };
        if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        result
            .ok()
            .context("open the current-user Chrome registration")?;
        Ok(Some(Self(key)))
    }

    fn create(path: &str) -> Result<(Self, bool)> {
        let path = wide(path);
        let mut key = HKEY::default();
        let mut disposition = REG_CREATE_KEY_DISPOSITION::default();
        // SAFETY: terminated path and writable outputs; no security/remote/hive override.
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE | KEY_WOW64_32KEY,
                None,
                &mut key,
                Some(&mut disposition),
            )
            .ok()?;
        }
        Ok((Self(key), disposition == REG_CREATED_NEW_KEY))
    }

    fn default_value(&self) -> Result<Option<String>> {
        let mut kind = REG_VALUE_TYPE::default();
        let mut size = 0;
        // SAFETY: first query sizes the value; the second fills aligned owned storage.
        unsafe {
            let result = RegQueryValueExW(
                self.0,
                PCWSTR::null(),
                None,
                Some(&mut kind),
                None,
                Some(&mut size),
            );
            if result == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            result.ok()?;
            if kind != REG_SZ || size < 2 || size % 2 != 0 || size > 32768 {
                bail!("registration default must be a bounded REG_SZ; preserve the foreign value and resolve it manually");
            }
            let mut text = vec![0u16; size as usize / 2];
            RegQueryValueExW(
                self.0,
                PCWSTR::null(),
                None,
                Some(&mut kind),
                Some(text.as_mut_ptr().cast()),
                Some(&mut size),
            )
            .ok()?;
            text.truncate(size as usize / 2);
            if kind != REG_SZ || text.last() != Some(&0) || text[..text.len() - 1].contains(&0) {
                bail!("invalid registration REG_SZ; preserve the foreign value and resolve it manually");
            }
            Ok(Some(
                String::from_utf16(&text[..text.len() - 1])
                    .context("registration path must be Unicode")?,
            ))
        }
    }

    fn set_default(&self, path: &str) -> Result<()> {
        let bytes: Vec<u8> = wide(path)
            .iter()
            .flat_map(|character| character.to_le_bytes())
            .collect();
        // SAFETY: bytes contain a terminated UTF-16 REG_SZ and live for the call.
        unsafe {
            RegSetValueExW(self.0, PCWSTR::null(), 0, REG_SZ, Some(&bytes)).ok()?;
        }
        Ok(())
    }

    fn empty(&self) -> Result<bool> {
        let mut subkeys = 0;
        let mut values = 0;
        // SAFETY: only counts are requested from a live query-capable handle.
        unsafe {
            RegQueryInfoKeyW(
                self.0,
                PWSTR::null(),
                None,
                None,
                Some(&mut subkeys),
                None,
                None,
                Some(&mut values),
                None,
                None,
                None,
                None,
            )
            .ok()?;
        }
        Ok(subkeys == 0 && values == 0)
    }
}

struct Manager {
    directory: PathBuf,
    key_path: String,
}
struct Inspection {
    state: &'static str,
    detail: String,
    config: Option<HostConfig>,
}

impl Manager {
    fn current_user() -> Result<Self> {
        // SAFETY: null token resolves the current user; Shell allocates with CoTaskMem.
        let directory = unsafe {
            let path =
                SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, HANDLE::default())?;
            let result = path.to_string();
            CoTaskMemFree(Some(path.0.cast()));
            PathBuf::from(result?)
                .join("Nova")
                .join("ChromeNativeMessaging")
        };
        Ok(Self {
            directory,
            key_path: CHROME_LEAF.into(),
        })
    }

    fn file(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
    fn manifest_path(&self) -> Result<String> {
        if !self.directory.is_absolute() {
            bail!("installation directory must be absolute");
        }
        Ok(self
            .file(MANIFEST_NAME)
            .to_str()
            .context("manifest path must be Unicode")?
            .into())
    }
    fn manifest(&self, config: &HostConfig) -> Value {
        json!({"name":HOST_NAME,"description":"Nova Chrome native messaging host",
            "path":self.file(BINARY_NAME),"type":"stdio",
            "allowed_origins":[format!("chrome-extension://{}/",config.extension_id)]})
    }
    fn registration(&self, view: REG_SAM_FLAGS) -> Result<Option<String>> {
        Key::open(&self.key_path, KEY_QUERY_VALUE, view)?
            .map(|key| key.default_value())
            .transpose()
            .map(Option::flatten)
    }
    fn remove_empty_key(&self) -> Result<()> {
        if let Some(key) = Key::open(&self.key_path, KEY_READ, KEY_WOW64_32KEY)? {
            if key.empty()? {
                drop(key);
                let path = wide(&self.key_path);
                // SAFETY: only this exact empty leaf is removed, never its parents.
                unsafe {
                    RegDeleteKeyExW(
                        HKEY_CURRENT_USER,
                        PCWSTR(path.as_ptr()),
                        KEY_WOW64_32KEY.0,
                        0,
                    )
                    .ok()?;
                }
            }
        }
        Ok(())
    }

    fn inspect(&self) -> Result<Inspection> {
        let directory_exists = metadata(&self.directory)?.is_some();
        let registered = self.registration(KEY_WOW64_32KEY)?;
        if self.registration(KEY_WOW64_64KEY)? != registered {
            bail!("Chrome registry views disagree; preserve both values and resolve the conflict manually");
        }
        if metadata(&self.file(CONFIG_NAME))?.is_none() {
            if registered.is_some()
                || metadata(&self.file(BINARY_NAME))?.is_some()
                || metadata(&self.file(MANIFEST_NAME))?.is_some()
            {
                bail!("ownership record is missing; preserve existing objects and resolve their ownership manually");
            }
            return Ok(Inspection {
                state: "absent",
                detail: if directory_exists {
                    "No owned Chrome host is registered; preserve the unmarked directory and resolve its ownership before installing".into()
                } else {
                    "No owned Chrome host is registered; run nova chrome-host install".into()
                },
                config: None,
            });
        }
        let config = read_host_config(&self.file(CONFIG_NAME))?;
        let mut missing = Vec::new();
        let binary = self.file(BINARY_NAME);
        if metadata(&binary)?.is_none() {
            missing.push(BINARY_NAME);
        } else if digest(&binary)? != config.binary_sha256 {
            bail!("{BINARY_NAME} differs from the owned binary; preserve it and resolve the replacement manually");
        }
        let manifest = self.file(MANIFEST_NAME);
        if metadata(&manifest)?.is_none() {
            missing.push(MANIFEST_NAME);
        } else if read_json(&manifest)? != self.manifest(&config) {
            bail!("{MANIFEST_NAME} differs from the owned manifest; preserve it and resolve the replacement manually");
        }
        match registered {
            Some(path) if path != self.manifest_path()? => bail!("registration points to a foreign manifest; preserve it and resolve the replacement manually"),
            None => missing.push("registry default"),
            _ => {}
        }
        Ok(Inspection {
            state: if missing.is_empty() {
                "installed"
            } else {
                "incomplete"
            },
            detail: if missing.is_empty() {
                "Owned registration verified; explicitly configure the managed MCP process using the reported environment".into()
            } else {
                format!(
                    "Missing {}; run nova chrome-host uninstall, then install again",
                    missing.join(", ")
                )
            },
            config: Some(config),
        })
    }

    fn report(&self) -> Value {
        let inspection = self.inspect().unwrap_or_else(|error| Inspection {
            state: "conflict",
            detail: format!("{error:#}"),
            config: None,
        });
        let mut report = json!({"state":inspection.state,"detail":inspection.detail});
        if let Some(config) = inspection.config {
            let mut environment = json!({"NOVA_CHROME_EXTENSION_ID":config.extension_id});
            if let Some(pipe) = config.pipe {
                environment["NOVA_CHROME_PIPE"] = json!(pipe);
            }
            report["extensionId"] = json!(config.extension_id);
            report["manifestPath"] = json!(self.file(MANIFEST_NAME));
            report["mcp"] = json!({"command":"nova","args":["mcp"],"env":environment});
        }
        report
    }

    fn install(&self, source: &Path, id: &str, pipe: Option<&Path>) -> Result<Value> {
        if !source.is_absolute() {
            bail!("--host-binary must be an absolute already-built executable path");
        }
        let config = HostConfig::new(id, pipe, digest(source)?)?;
        self.manifest_path()?;
        let previous = self.inspect().context(
            "installation conflict; preserve existing objects and resolve their ownership",
        )?;
        if let Some(existing) = previous.config {
            if previous.state == "installed" && existing == config {
                return Ok(self.report());
            }
            bail!("installation differs or is incomplete; run nova chrome-host uninstall before reinstalling");
        }
        if metadata(&self.directory)?.is_some() {
            bail!("existing unmarked installation directory; preserve it and resolve ownership before installing");
        }
        let parent = self
            .directory
            .parent()
            .context("installation directory has no parent")?;
        metadata(parent)?;
        fs::create_dir_all(parent)?;
        fs::create_dir(&self.directory).context(
            "create fresh installation directory; concurrent installations must not replace it",
        )?;
        let mut created = Vec::new();
        let result = (|| -> Result<()> {
            let binary = self.file(BINARY_NAME);
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&binary)?;
            created.push(binary.clone());
            std::io::copy(&mut File::open(source)?, &mut output)?;
            output.sync_all()?;
            drop(output);
            if digest(&binary)? != config.binary_sha256 {
                bail!("source binary changed while copying; retry with a stable built executable");
            }
            for (name, value) in [
                (CONFIG_NAME, config.json()),
                (MANIFEST_NAME, self.manifest(&config)),
            ] {
                let path = self.file(name);
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                created.push(path);
                output.write_all(&serde_json::to_vec_pretty(&value)?)?;
                output.sync_all()?;
            }
            let (key, new_key) = Key::create(&self.key_path)?;
            let published = (|| -> Result<()> {
                if key.default_value()?.is_some() {
                    bail!("registration appeared during install; preserve it and resolve ownership manually");
                }
                key.set_default(&self.manifest_path()?)
            })();
            drop(key);
            if published.is_err() && new_key {
                self.remove_empty_key()?;
            }
            published?;
            Ok(())
        })();
        if let Err(error) = result {
            for path in created.into_iter().rev() {
                if metadata(&path).is_ok_and(|info| info.is_some()) {
                    let _ = fs::remove_file(path);
                }
            }
            let _ = fs::remove_dir(&self.directory); // empty only; no parent cleanup
            return Err(error)
                .context("Chrome host install failed; no existing object was taken over");
        }
        Ok(self.report())
    }

    fn uninstall(&self) -> Result<Value> {
        let inspection = self.inspect().context(
            "uninstall refused; preserve conflicting objects and resolve their ownership manually",
        )?;
        if inspection.config.is_none() {
            return Ok(self.report());
        }
        if let Some(key) = Key::open(
            &self.key_path,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            KEY_WOW64_32KEY,
        )? {
            if let Some(path) = key.default_value()? {
                if path != self.manifest_path()? {
                    bail!("foreign registration appeared; preserving it");
                }
                // SAFETY: the queried default exactly points at our verified manifest.
                unsafe {
                    RegDeleteValueW(key.0, PCWSTR::null()).ok()?;
                }
            }
        }
        self.remove_empty_key()?;
        // Keep the valid record until all payload removals succeed, so a sharing
        // failure or a previously removed payload can be retried after host exit.
        for name in [BINARY_NAME, MANIFEST_NAME, CONFIG_NAME] {
            let path = self.file(name);
            if metadata(&path)?.is_some() {
                fs::remove_file(&path).with_context(|| {
                    format!(
                        "remove {}; close the owned host and retry nova chrome-host uninstall",
                        path.display()
                    )
                })?;
            }
        }
        if fs::read_dir(&self.directory)?.next().is_none() {
            fs::remove_dir(&self.directory)?;
        }
        Ok(self.report())
    }
}

/// Copy and register one explicit already-built host for current-user Google Chrome.
pub fn install_chrome_host(
    source: &Path,
    extension_id: &str,
    pipe: Option<&Path>,
) -> Result<Value> {
    Manager::current_user()?.install(source, extension_id, pipe)
}
/// Inspect fixed owned objects only; this never connects to the Chrome broker.
pub fn chrome_host_status() -> Result<Value> {
    Ok(Manager::current_user()?.report())
}
/// Remove only verified owned objects, preserving unrelated files/values/subkeys.
pub fn uninstall_chrome_host() -> Result<Value> {
    Manager::current_user()?.uninstall()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{encode_native, encode_ndjson, NativeDecoder, NdjsonDecoder};
    use crate::AppBridgeListener;
    use std::os::windows::ffi::OsStringExt;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";
    const OTHER: &str = "pppppppppppppppppppppppppppppppp";
    const DEADLINE: Duration = Duration::from_secs(5);

    fn supplied(name: &str) -> PathBuf {
        let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| {
            panic!("build both Windows fixture binaries and set {name} to its absolute path")
        }));
        assert!(
            path.is_absolute() && path.is_file(),
            "{name} must name an existing absolute fixture binary"
        );
        path
    }

    struct Fixture {
        manager: Manager,
        root: PathBuf,
        registry_root: String,
        pipe: PathBuf,
        source: PathBuf,
        nova: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let source = supplied("NOVA_TEST_CHROME_HOST");
            let nova = supplied("NOVA_TEST_NOVA_EXE");
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let suffix = format!(
                "{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let root = std::env::temp_dir().join(format!("Nova registration 空 {suffix}"));
            fs::create_dir(&root).unwrap();
            let registry_root = format!(r"Software\Nova\Tests\{suffix}");
            let manager = Manager {
                directory: root.join("installation"),
                key_path: format!(r"{registry_root}\{CHROME_LEAF}"),
            };
            Self {
                manager,
                root,
                registry_root,
                pipe: PathBuf::from(format!(r"\\.\pipe\nova-chrome-registration-{suffix}")),
                source,
                nova,
            }
        }
        fn install(&self) {
            assert_eq!(
                self.manager
                    .install(&self.source, ID, Some(&self.pipe))
                    .unwrap()["state"],
                "installed"
            );
        }
        fn host(&self, id: &str) -> Command {
            let manifest = self.manager.registration(KEY_WOW64_32KEY).unwrap().unwrap();
            let executable = read_json(Path::new(&manifest)).unwrap()["path"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut command = Command::new(executable);
            command
                .arg(format!("chrome-extension://{id}/"))
                .current_dir(&self.root)
                .env_remove("NOVA_CHROME_EXTENSION_ID")
                .env_remove("NOVA_CHROME_PIPE");
            command
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Every name here belongs to this unique fixture namespace, including
            // deliberate foreign replacements. Never sweep a user namespace.
            for name in [BINARY_NAME, MANIFEST_NAME, CONFIG_NAME, "sentinel"] {
                let _ = fs::remove_file(self.manager.file(name));
            }
            let _ = fs::remove_dir(&self.manager.directory);
            let sentinel = format!(r"{}\sentinel", self.manager.key_path);
            for path in [
                self.manager.key_path.clone(),
                format!(r"{}\Sibling", self.registry_root),
            ] {
                if let Ok(Some(key)) = Key::open(&path, KEY_WRITE, KEY_WOW64_32KEY) {
                    unsafe {
                        let _ = RegDeleteValueW(key.0, PCWSTR::null());
                        let _ = RegDeleteValueW(key.0, windows::core::w!("sentinel"));
                    }
                }
            }
            for path in [sentinel, format!(r"{}\Sibling", self.registry_root)] {
                let _ = Manager {
                    directory: self.root.clone(),
                    key_path: path,
                }
                .remove_empty_key();
            }
            let mut path = self.manager.key_path.clone();
            loop {
                let _ = Manager {
                    directory: self.root.clone(),
                    key_path: path.clone(),
                }
                .remove_empty_key();
                if path == self.registry_root {
                    break;
                }
                path.truncate(path.rfind('\\').unwrap());
            }
            let _ = fs::remove_dir(&self.root);
        }
    }

    struct Process {
        child: Child,
        input: Option<ChildStdin>,
        output: mpsc::Receiver<Value>,
        reader: Option<std::thread::JoinHandle<Result<()>>>,
        native: bool,
    }
    impl Process {
        fn start(mut command: Command, native: bool) -> Self {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let input = child.stdin.take();
            let mut stdout = child.stdout.take().unwrap();
            let (sender, output) = mpsc::channel();
            let reader = std::thread::spawn(move || -> Result<()> {
                let mut framed = NativeDecoder::default();
                let mut lines = NdjsonDecoder::default();
                let mut bytes = [0u8; 4096];
                loop {
                    let count = stdout.read(&mut bytes)?;
                    if count == 0 {
                        return if native {
                            framed.finish()
                        } else {
                            lines.finish()
                        };
                    }
                    for value in if native {
                        framed.push(&bytes[..count])?
                    } else {
                        lines.push(&bytes[..count])?
                    } {
                        if sender.send(value).is_err() {
                            return Ok(());
                        }
                    }
                }
            });
            Self {
                child,
                input,
                output,
                reader: Some(reader),
                native,
            }
        }
        fn send(&mut self, value: &Value) {
            let bytes = if self.native {
                encode_native(value)
            } else {
                encode_ndjson(value)
            }
            .unwrap();
            self.input.as_mut().unwrap().write_all(&bytes).unwrap();
        }
        fn receive(&self) -> Value {
            self.output
                .recv_timeout(DEADLINE)
                .expect("fixture frame deadline exceeded")
        }
        fn response(&self, id: u64) -> Value {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let value = self
                    .output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap();
                if value["id"] == id {
                    return value;
                }
            }
        }
        fn finish(&mut self, success: bool) -> String {
            let deadline = Instant::now() + DEADLINE;
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert_eq!(status.success(), success);
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "owned fixture process did not exit"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            self.reader
                .take()
                .unwrap()
                .join()
                .unwrap()
                .expect("stdout must contain only complete protocol frames");
            let mut diagnostic = String::new();
            self.child
                .stderr
                .take()
                .unwrap()
                .take(8193)
                .read_to_string(&mut diagnostic)
                .unwrap();
            assert!(diagnostic.len() <= 8192, "fixture stderr must be bounded");
            diagnostic
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            if self.child.try_wait().is_ok_and(|status| status.is_none()) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait(); // only this fixture's Child, including assertion failures
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    #[test]
    fn windows_registration_install_status_idempotency_and_owned_uninstall() {
        let fixture = Fixture::new();
        let manager = &fixture.manager;
        assert_eq!(manager.report()["state"], "absent");
        assert_eq!(
            manager.install(&fixture.source, ID, None).unwrap()["state"],
            "installed"
        );
        let config = read_host_config(&manager.file(CONFIG_NAME)).unwrap();
        assert!(config.pipe.is_none());
        assert_eq!(
            config.resolved_pipe().unwrap(),
            default_pipe_path().unwrap()
        );
        let modified = fs::metadata(manager.file(BINARY_NAME))
            .unwrap()
            .modified()
            .unwrap();
        manager.install(&fixture.source, ID, None).unwrap();
        assert_eq!(
            fs::metadata(manager.file(BINARY_NAME))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
            assert_eq!(
                manager.registration(view).unwrap().unwrap(),
                manager.manifest_path().unwrap()
            );
        }
        let manifest = read_json(&manager.file(MANIFEST_NAME)).unwrap();
        assert!(Path::new(manifest["path"].as_str().unwrap()).is_absolute());
        assert_eq!(
            manifest["allowed_origins"],
            json!([format!("chrome-extension://{ID}/")])
        );
        assert!(manager.install(&fixture.source, OTHER, None).is_err());
        fs::write(manager.file("sentinel"), b"unrelated file").unwrap();
        let (key, _) = Key::create(&manager.key_path).unwrap();
        unsafe {
            RegSetValueExW(
                key.0,
                windows::core::w!("sentinel"),
                0,
                REG_BINARY,
                Some(b"unrelated value"),
            )
            .ok()
            .unwrap();
        }
        drop(key);
        let (subkey, _) = Key::create(&format!(r"{}\sentinel", manager.key_path)).unwrap();
        drop(subkey);
        let sibling = format!(r"{}\Sibling", fixture.registry_root);
        let (key, _) = Key::create(&sibling).unwrap();
        key.set_default("unrelated sibling").unwrap();
        drop(key);
        assert_eq!(manager.uninstall().unwrap()["state"], "absent");
        assert_eq!(manager.uninstall().unwrap()["state"], "absent");
        let report = manager.report();
        assert!(report["detail"]
            .as_str()
            .unwrap()
            .contains("resolve its ownership before installing"));
        assert!(manager.registration(KEY_WOW64_32KEY).unwrap().is_none());
        for name in [BINARY_NAME, MANIFEST_NAME, CONFIG_NAME] {
            assert!(!manager.file(name).exists());
        }
        assert_eq!(
            fs::read(manager.file("sentinel")).unwrap(),
            b"unrelated file"
        );
        let key = Key::open(&manager.key_path, KEY_READ, KEY_WOW64_32KEY)
            .unwrap()
            .unwrap();
        assert!(!key.empty().unwrap());
        let mut bytes = [0u8; 32];
        let mut length = bytes.len() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        // SAFETY: query only the exact sentinel this fixture created.
        unsafe {
            RegQueryValueExW(
                key.0,
                windows::core::w!("sentinel"),
                None,
                Some(&mut kind),
                Some(bytes.as_mut_ptr()),
                Some(&mut length),
            )
            .ok()
            .unwrap();
        }
        assert_eq!(kind, REG_BINARY);
        assert_eq!(&bytes[..length as usize], b"unrelated value");
        assert!(Key::open(
            &format!(r"{}\sentinel", manager.key_path),
            KEY_READ,
            KEY_WOW64_64KEY
        )
        .unwrap()
        .is_some());
        assert_eq!(
            Key::open(&sibling, KEY_READ, KEY_WOW64_32KEY)
                .unwrap()
                .unwrap()
                .default_value()
                .unwrap()
                .unwrap(),
            "unrelated sibling"
        );
    }

    #[test]
    fn windows_registration_refuses_foreign_objects_and_allows_owned_partial_retry() {
        let fixture = Fixture::new();
        let manager = &fixture.manager;
        for (source, id, pipe) in [
            (fixture.source.as_path(), "bad", None),
            (
                fixture.source.as_path(),
                ID,
                Some(Path::new(r"\\server\pipe\foreign")),
            ),
            (Path::new(r"C:\missing-nova-host.exe"), ID, None),
        ] {
            assert!(manager.install(source, id, pipe).is_err());
            assert!(!manager.directory.exists());
        }
        let (key, _) = Key::create(&manager.key_path).unwrap();
        key.set_default(r"C:\foreign\do-not-read.json").unwrap();
        assert!(manager.install(&fixture.source, ID, None).is_err());
        assert!(manager.uninstall().is_err());
        assert_eq!(
            key.default_value().unwrap().unwrap(),
            r"C:\foreign\do-not-read.json"
        );
        unsafe {
            RegSetValueExW(key.0, PCWSTR::null(), 0, REG_BINARY, Some(b"foreign type"))
                .ok()
                .unwrap();
        }
        assert_eq!(manager.report()["state"], "conflict");
        unsafe {
            RegDeleteValueW(key.0, PCWSTR::null()).ok().unwrap();
        }
        drop(key);
        fs::create_dir(&manager.directory).unwrap();
        assert!(manager.install(&fixture.source, ID, None).is_err());
        fs::remove_dir(&manager.directory).unwrap();
        fixture.install();
        fs::remove_file(manager.file(BINARY_NAME)).unwrap();
        assert_eq!(manager.report()["state"], "incomplete");
        assert_eq!(manager.uninstall().unwrap()["state"], "absent");
        fixture.install();
        fs::write(manager.file(MANIFEST_NAME), b"foreign replacement").unwrap();
        assert!(manager.uninstall().is_err());
        assert_eq!(
            fs::read(manager.file(MANIFEST_NAME)).unwrap(),
            b"foreign replacement"
        );
        assert!(manager.file(CONFIG_NAME).exists());
        fs::write(
            manager.file(MANIFEST_NAME),
            serde_json::to_vec(
                &manager.manifest(&read_host_config(&manager.file(CONFIG_NAME)).unwrap()),
            )
            .unwrap(),
        )
        .unwrap();
        let (key, _) = Key::create(&manager.key_path).unwrap();
        key.set_default(r"C:\replacement.json").unwrap();
        drop(key);
        assert_eq!(manager.report()["state"], "conflict");
        assert!(manager.uninstall().is_err());
        assert_eq!(
            manager.registration(KEY_WOW64_32KEY).unwrap().unwrap(),
            r"C:\replacement.json"
        );
    }

    fn hello(id: &str) -> Value {
        json!({"protocolVersion":1,"kind":"hello","role":"chrome_extension","extensionId":id})
    }
    fn accept(listener: &AppBridgeListener) -> crate::AppBridgeConnection {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(connection) = listener.try_accept().unwrap() {
                return connection;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn windows_registered_host_config_is_exact_and_environment_never_falls_back() {
        let fixture = Fixture::new();
        fixture.install();
        let path = fixture.manager.file(CONFIG_NAME);
        let owned = fs::read(&path).unwrap();
        let listener = AppBridgeListener::bind(&fixture.pipe).unwrap();
        let mut wrong = Process::start(fixture.host(OTHER), true);
        assert!(wrong.finish(false).contains("origin differs"));
        assert!(listener.try_accept().unwrap().is_none());
        let mut host = Process::start(fixture.host(ID), true);
        let mut app = accept(&listener);
        assert_eq!(app.receive().unwrap().unwrap()["extensionId"], ID);
        host.send(&hello(OTHER));
        assert!(host.finish(false).contains("handshake identity"));
        drop(app);
        for bytes in [
            b"{".to_vec(),
            vec![b' '; JSON_LIMIT as usize + 1],
            b"{\"schemaVersion\":2}".to_vec(),
        ] {
            fs::write(&path, bytes).unwrap();
            assert!(read_host_config(&path).is_err());
        }
        let mut command = fixture.host(ID);
        command
            .env("NOVA_CHROME_EXTENSION_ID", ID)
            .env("NOVA_CHROME_PIPE", &fixture.pipe);
        let mut authoritative = Process::start(command, true);
        let mut app = accept(&listener);
        assert_eq!(app.receive().unwrap().unwrap()["extensionId"], ID);
        drop(app);
        assert!(authoritative.finish(true).len() < 8192);
        fs::write(&path, &owned).unwrap();
        for (name, value, complete) in [
            (
                "NOVA_CHROME_PIPE",
                fixture.pipe.as_os_str().to_owned(),
                false,
            ),
            ("NOVA_CHROME_EXTENSION_ID", "bad".into(), false),
            (
                "NOVA_CHROME_EXTENSION_ID",
                std::ffi::OsString::from_wide(&[0xd800]),
                false,
            ),
            (
                "NOVA_CHROME_PIPE",
                std::ffi::OsString::from_wide(&[0xd800]),
                true,
            ),
        ] {
            let mut command = fixture.host(ID);
            command.env(name, value);
            if complete {
                command.env("NOVA_CHROME_EXTENSION_ID", ID);
            }
            let mut host = Process::start(command, true);
            assert!(!host.finish(false).is_empty());
            assert!(listener.try_accept().unwrap().is_none());
        }
        fs::write(&path, b"{").unwrap();
        let mut host = Process::start(fixture.host(ID), true);
        assert!(host.finish(false).contains("configuration unavailable"));
        fs::remove_file(&path).unwrap();
        let mut host = Process::start(fixture.host(ID), true);
        assert!(host.finish(false).contains("configuration unavailable"));
        fs::write(path, owned).unwrap();
    }

    #[test]
    fn windows_registered_copied_host_roundtrips_real_managed_mcp_and_closes_with_stdin_open() {
        let fixture = Fixture::new();
        fixture.install();
        let mut command = Command::new(&fixture.nova);
        command
            .arg("mcp")
            .env("NOVA_CHROME_EXTENSION_ID", ID)
            .env("NOVA_CHROME_PIPE", &fixture.pipe)
            .env_remove("RUST_LOG");
        let mut mcp = Process::start(command, false);
        mcp.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"registration-fixture","version":"1"}}}));
        assert!(mcp.response(1)["result"]["serverInfo"].is_object());
        mcp.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        mcp.send(&json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
        assert_eq!(mcp.response(2)["result"], json!({}));
        let mut host = Process::start(fixture.host(ID), true);
        host.send(&hello(ID));
        let driver = std::thread::spawn(move || {
            let request = host.receive();
            assert_eq!(request["action"], "status");
            host.send(&json!({"protocolVersion":1,"kind":"result","requestId":request["requestId"],"action":"status","status":"ok","epoch":3,
                "receipt":{"receiptId":"registration-receipt","expiresAt":10000},"result":{"paired":false}}));
            let receipt = host.receive();
            assert_eq!(receipt["kind"], "receipt");
            assert_eq!(receipt["requestId"], request["requestId"]);
            host
        });
        let deadline = Instant::now() + DEADLINE;
        loop {
            mcp.send(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"chrome_status","arguments":{}}}));
            let response = mcp.response(3);
            if response.get("error").is_none() && response["result"]["isError"] != true {
                break;
            }
            assert!(Instant::now() < deadline, "{response}");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut host = driver.join().unwrap();
        drop(mcp.input.take());
        mcp.finish(true);
        assert!(host.input.is_some());
        let diagnostic = host.finish(true);
        assert!(
            diagnostic.contains("extension_to_app") && !diagnostic.contains("registration-receipt")
        );
        assert!(AppBridgeListener::bind(&fixture.pipe).is_ok());
        assert_eq!(fixture.manager.uninstall().unwrap()["state"], "absent");
    }
}
