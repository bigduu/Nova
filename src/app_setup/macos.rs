use super::{AppCommand, SetupReport, SetupState};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod local_sign;

const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const APP: &str = "Nova.app";

struct Locations {
    home: PathBuf,
    system_app: PathBuf,
}

impl Locations {
    fn user_app(&self) -> PathBuf {
        self.home.join("Applications").join(APP)
    }

    fn current_user() -> Result<Self> {
        // Resolve the account's home through the OS rather than accepting an
        // environment-controlled installation destination.
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut result = std::ptr::null_mut();
        // SAFETY: all pointers reference live writable buffers; getpwuid_r
        // initializes record only when it returns zero and a non-null result.
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        ensure!(
            code == 0 && !result.is_null(),
            "Cannot resolve the current account's home."
        );
        // SAFETY: the successful call above initialized record; pw_dir points
        // into buffer and is NUL-terminated for its remaining lifetime.
        let home = unsafe {
            let record = record.assume_init();
            ensure!(
                !record.pw_dir.is_null(),
                "The account has no home directory."
            );
            PathBuf::from(std::ffi::OsStr::from_bytes(
                CStr::from_ptr(record.pw_dir).to_bytes(),
            ))
        };
        ensure!(home.is_absolute(), "The account home must be absolute.");
        let home = home.canonicalize().context("Resolve the account home")?;
        validate_owned_directory(&home)?;
        Ok(Self {
            home,
            system_app: PathBuf::from("/Applications/Nova.app"),
        })
    }
}

pub(super) fn run(command: &AppCommand) -> SetupReport {
    match Locations::current_user().and_then(|locations| execute(command, &locations)) {
        Ok(report) => report,
        Err(error) => SetupReport::new(
            SetupState::Error,
            false,
            &format!("{error:#}"),
            "Keep existing apps unchanged. Check the trusted archive/checksum. For --local-sign failures, inspect the login keychain manually for duplicate, expired, certificate-only or inaccessible Zenith Nova Code Signing entries; a failed creation may have imported a certificate. Do not delete/recreate the stable identity automatically. Setup never grants privacy permissions.",
        ),
    }
}

fn execute(command: &AppCommand, locations: &Locations) -> Result<SetupReport> {
    execute_with_signer(command, locations, &local_sign::NativeSigner)
}

fn execute_with_signer(
    command: &AppCommand,
    locations: &Locations,
    signer: &dyn local_sign::Signer,
) -> Result<SetupReport> {
    let user_app = locations.user_app();
    let applications = user_app.parent().context("Missing Applications parent")?;
    if fs::symlink_metadata(applications).is_ok() {
        validate_owned_directory(applications)?;
    }
    for path in [&user_app, &locations.system_app] {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                let identity = inspect_bundle(path)
                    .with_context(|| format!("Inspect existing Nova.app at {}", path.display()))?;
                let installing = matches!(command, AppCommand::Install { .. });
                let mut report = identity.report(path);
                if installing {
                    report.state = SetupState::AlreadyInstalled;
                    report.success = false;
                    report.message =
                        "Nova.app already exists. Initial setup refuses to replace it.".into();
                    report.next_action = "Use the existing app or manage its upgrade separately; reconnect only Nova MCP when needed.".into();
                }
                return Ok(report);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Inspect the canonical app path"),
        }
    }
    match command {
        AppCommand::Status => {
            let mut report = SetupReport::new(
                SetupState::Missing,
                true,
                "No Nova.app exists at either canonical installation path.",
                "Obtain the matching app ZIP and SHA256 from a trusted release, then explicitly run nova app install --archive <path> --sha256 <hex>.",
            );
            report.path = Some(user_app);
            Ok(report)
        }
        AppCommand::Install {
            archive,
            sha256,
            local_sign,
        } => install_with_signer(archive, sha256, locations, *local_sign, signer),
    }
}

fn validate_owned_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Refusing a non-directory or symlink at {}",
        path.display()
    );
    ensure!(
        metadata.uid() == uid && metadata.mode() & 0o022 == 0,
        "Directory must belong to the current account and not be writable by others: {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
fn install(archive: &Path, expected: &str, locations: &Locations) -> Result<SetupReport> {
    install_with_signer(
        archive,
        expected,
        locations,
        false,
        &local_sign::NativeSigner,
    )
}

fn install_with_signer(
    archive: &Path,
    expected: &str,
    locations: &Locations,
    local_sign: bool,
    signer: &dyn local_sign::Signer,
) -> Result<SetupReport> {
    ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "SHA256 must be exactly 64 hexadecimal digits."
    );
    let applications = locations.home.join("Applications");
    match fs::symlink_metadata(&applications) {
        Ok(_) => validate_owned_directory(&applications)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o755).create(&applications)?;
            validate_owned_directory(&applications)?;
        }
        Err(error) => return Err(error).context("Inspect Applications directory"),
    }
    let staging = tempfile::Builder::new()
        .prefix(".nova-app-setup-")
        .tempdir_in(&applications)?;
    let snapshot_path = staging.path().join("source.zip");
    let mut source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(archive)
        .context("Open a regular local app archive")?;
    let metadata = source.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_ARCHIVE,
        "App archive must be a regular ZIP file no larger than 128 MiB."
    );
    let mut snapshot = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&snapshot_path)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= MAX_ARCHIVE, "App archive exceeds 128 MiB.");
        hasher.update(&buffer[..count]);
        snapshot.write_all(&buffer[..count])?;
    }
    ensure!(
        format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(expected),
        "App archive SHA256 mismatch; nothing was installed."
    );
    snapshot.sync_all()?;
    let canonical_path = staging.path().join("validated.zip");
    normalize_archive(File::open(&snapshot_path)?, &canonical_path)?;
    // Native extraction sees only the validated, freshly serialized ZIP, never
    // unreferenced local headers or alternate paths in the supplied archive.
    // Retain its download quarantine; removing it is deliberately unsupported.
    let canonical = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&canonical_path)?;
    copy_quarantine(&source, &canonical)?;
    let extracted = staging.path().join("extracted");
    fs::create_dir(&extracted)?;
    native(
        "/usr/bin/ditto",
        &["-x", "-k", "--rsrc", "--extattr", "--qtn"],
        &[&canonical_path, &extracted],
    )?;
    let app = extracted.join(APP);
    let mut identity = inspect_bundle(&app)?;
    ensure!(
        identity.version == env!("CARGO_PKG_VERSION"),
        "App version {} does not match this connector ({}). Obtain a matching release.",
        identity.version,
        env!("CARGO_PKG_VERSION")
    );
    if local_sign {
        // Every archive, tree, original signature and version check above must
        // pass before the explicit mode can access a mutable signing identity.
        local_sign::validate_code_layout(&app)?;
        signer.sign(&app, &locations.home)?;
        identity = inspect_bundle(&app)?;
        ensure!(
            identity.signing == "local_self_signed",
            "Local signature postcondition failed."
        );
    }
    publish_initial(&extracted, &applications)?;
    let mut report = identity.report(&applications.join(APP));
    report.message = "Verified Nova.app was installed without replacing an existing app.".into();
    Ok(report)
}

fn normalize_archive(source: File, target: &Path) -> Result<()> {
    use zip::write::SimpleFileOptions;
    let mut archive = zip::ZipArchive::new(source).context("Read app ZIP")?;
    ensure!(
        !archive.is_empty() && archive.len() <= MAX_ENTRIES,
        "App ZIP has an invalid entry count."
    );
    ensure!(
        !archive.has_overlapping_files()?,
        "App ZIP has overlapping entries."
    );
    let mut writer = zip::ZipWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?,
    );
    let mut paths = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).context("Read ZIP entry")?;
        let name = std::str::from_utf8(entry.name_raw()).context("ZIP paths must be UTF-8")?;
        ensure!(
            name.is_ascii() && !name.contains(['\\', ':', '\0']) && !name.starts_with('/'),
            "Unsupported ZIP path: {name:?}"
        );
        let name = name.trim_end_matches('/');
        let components: Vec<_> = name.split('/').collect();
        ensure!(
            components
                .iter()
                .all(|part| !part.is_empty() && *part != "." && *part != ".."),
            "Unsafe ZIP path: {name:?}"
        );
        ensure!(
            components[0] == APP
                || (components[0] == "__MACOSX"
                    && (components.len() == 1
                        || components[1] == APP
                        || components[1] == "._Nova.app")),
            "ZIP must contain only Nova.app and its AppleDouble metadata."
        );
        let directory = entry.is_dir();
        let metadata_entry = components[0] == "__MACOSX";
        let mode = entry
            .unix_mode()
            .unwrap_or(if directory { 0o040755 } else { 0o100644 });
        let kind = mode & u32::from(libc::S_IFMT);
        ensure!(
            !entry.is_symlink()
                && (kind == 0
                    || kind == u32::from(libc::S_IFREG)
                    || kind == u32::from(libc::S_IFDIR))
                && mode & 0o7000 == 0
                && (metadata_entry || mode & 0o022 == 0),
            "ZIP contains a link, special file, or unsafe permissions: {name:?}"
        );
        ensure!(!entry.encrypted(), "Encrypted app ZIPs are unsupported.");
        let key = name.to_ascii_lowercase();
        ensure!(
            paths.insert(key.clone(), directory).is_none(),
            "ZIP contains colliding paths: {name:?}"
        );
        for ancestor in
            (1..components.len()).map(|length| components[..length].join("/").to_ascii_lowercase())
        {
            ensure!(
                paths.get(&ancestor) != Some(&false),
                "ZIP has a file where a parent directory is required."
            );
        }
        if !directory {
            ensure!(
                !paths
                    .keys()
                    .any(|path| path.starts_with(&format!("{key}/"))),
                "ZIP has a conflicting file/directory path."
            );
        }
        total = total
            .checked_add(entry.size())
            .context("ZIP size overflow")?;
        ensure!(total <= MAX_UNPACKED, "App ZIP expands beyond 256 MiB.");
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            // ditto's synthetic __MACOSX container uses mode 0777. It is not
            // published; normalize that container while preserving app modes.
            .unix_permissions(if metadata_entry {
                if directory {
                    0o755
                } else {
                    0o644
                }
            } else {
                mode & 0o777
            });
        if directory {
            ensure!(entry.size() == 0, "ZIP directory has unexpected content.");
            writer.add_directory(name, options)?;
        } else {
            writer.start_file(name, options)?;
            let declared = entry.size();
            let written = io::copy(&mut entry.by_ref().take(declared + 1), &mut writer)?;
            ensure!(
                written == declared,
                "ZIP entry length does not match its declaration."
            );
        }
    }
    writer.finish()?.sync_all()?;
    Ok(())
}

fn copy_quarantine(source: &File, target: &File) -> Result<()> {
    let name = c"com.apple.quarantine";
    // SAFETY: valid file descriptors and NUL-terminated attribute name; a null
    // buffer asks macOS for the size without accessing memory.
    let size = unsafe {
        libc::fgetxattr(
            source.as_raw_fd(),
            name.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    if size < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOATTR) {
            return Ok(());
        }
        return Err(error).context("Read archive quarantine");
    }
    ensure!(size <= 64 * 1024, "Unexpected quarantine attribute size.");
    let mut value = vec![0_u8; size as usize];
    // SAFETY: both buffers are live for the syscalls and have the stated size.
    let count = unsafe {
        libc::fgetxattr(
            source.as_raw_fd(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    ensure!(
        count == size,
        "Archive quarantine changed while setup was reading it."
    );
    // SAFETY: valid descriptor, name and initialized buffer.
    let result = unsafe {
        libc::fsetxattr(
            target.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error()).context("Preserve archive quarantine");
    }
    Ok(())
}

struct BundleIdentity {
    version: String,
    signing: String,
}

impl BundleIdentity {
    fn report(self, path: &Path) -> SetupReport {
        let development = self.signing != "developer_id";
        let compatible = self.version == env!("CARGO_PKG_VERSION");
        let mut report = SetupReport::new(
            SetupState::Installed,
            true,
            "Nova.app bundle identity and code signature are valid; desktop readiness has not been tested.",
            if !compatible {
                "Use a connector and Nova.app from the same release. Initial setup does not upgrade existing apps."
            } else if self.signing == "local_self_signed" {
                "This app is locally self-signed, not Apple-notarized. Open Nova.app through macOS and grant required permissions explicitly, then reconnect only Nova MCP. Keep its stable keychain identity for later manual builds; upgrade permission continuity has not been tested."
            } else if development {
                "This is a development app, not a notarized production installation. Open Nova.app through macOS, handle any Gatekeeper prompt yourself, and grant permissions explicitly before reconnecting Nova MCP."
            } else {
                "Open Nova.app through macOS and grant required permissions explicitly, then reconnect only Nova MCP. Signature validation does not establish notarization or desktop readiness."
            },
        );
        report.path = Some(path.to_owned());
        report.version = Some(self.version);
        report.signing = Some(self.signing);
        report.development = Some(development);
        report.connector_compatible = Some(compatible);
        report
    }
}

fn inspect_bundle(app: &Path) -> Result<BundleIdentity> {
    validate_tree(app, &mut 0_u64, &mut 0_usize)?;
    let plist = app.join("Contents/Info.plist");
    ensure!(
        fs::metadata(&plist)?.len() <= 64 * 1024,
        "App Info.plist exceeds 64 KiB."
    );
    let output = native(
        "/usr/bin/plutil",
        &["-convert", "json", "-o", "-"],
        &[&plist],
    )?;
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Read Nova app identity")?;
    ensure!(
        info["CFBundleIdentifier"] == crate::app_service::BUNDLE_ID
            && info["CFBundleExecutable"] == "nova"
            && info["CFBundlePackageType"] == "APPL"
            && info["LSUIElement"] == true,
        "App is not the independent com.zenith.nova application."
    );
    let version = info["NovaReleaseVersion"]
        .as_str()
        .context("Missing NovaReleaseVersion")?;
    ensure!(
        !version.is_empty()
            && version.len() <= 64
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte)),
        "Invalid Nova release version."
    );
    let numeric = version.split(['-', '+']).next().unwrap_or_default();
    ensure!(
        info["CFBundleVersion"] == numeric && info["CFBundleShortVersionString"] == numeric,
        "App bundle versions disagree."
    );
    let executable = app.join("Contents/MacOS/nova");
    let metadata = fs::symlink_metadata(&executable)?;
    ensure!(
        metadata.is_file() && metadata.mode() & 0o111 != 0,
        "Nova app executable is missing or not executable."
    );
    for architecture in ["arm64", "x86_64"] {
        checked_output(
            "/usr/bin/lipo",
            Command::new("/usr/bin/lipo")
                .arg(&executable)
                .args(["-verify_arch", architecture])
                .output()?,
        )?;
    }
    native(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict"],
        &[app],
    )?;
    let signature = native("/usr/bin/codesign", &["--display", "--verbose=4"], &[app])?;
    let signature = String::from_utf8_lossy(&signature.stderr);
    ensure!(
        signature
            .lines()
            .any(|line| line == format!("Identifier={}", crate::app_service::BUNDLE_ID)),
        "Code signature has the wrong Nova identity."
    );
    let signing = if signature.lines().any(|line| line == "Signature=adhoc") {
        "ad_hoc"
    } else if signature
        .lines()
        .any(|line| line.starts_with("Authority=Developer ID Application:"))
    {
        "developer_id"
    } else if signature
        .lines()
        .any(|line| line == format!("Authority={}", local_sign::IDENTITY_NAME))
    {
        let requirements = native(
            "/usr/bin/codesign",
            &["--display", "--requirements", "-"],
            &[app],
        )?;
        let requirements = format!(
            "{}\n{}",
            String::from_utf8_lossy(&requirements.stdout),
            String::from_utf8_lossy(&requirements.stderr)
        );
        if local_sign::has_stable_requirement(&requirements) {
            "local_self_signed"
        } else {
            "other"
        }
    } else {
        "other"
    };
    Ok(BundleIdentity {
        version: version.into(),
        signing: signing.into(),
    })
}

fn validate_tree(path: &Path, bytes: &mut u64, entries: &mut usize) -> Result<()> {
    *entries += 1;
    ensure!(*entries <= MAX_ENTRIES, "App bundle has too many entries.");
    let metadata = fs::symlink_metadata(path).context("Inspect app bundle")?;
    ensure!(
        !metadata.file_type().is_symlink()
            && (metadata.is_file() || metadata.is_dir())
            && metadata.mode() & 0o7022 == 0,
        "App bundle contains a link, special file, or unsafe permissions: {}",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            validate_tree(&entry?.path(), bytes, entries)?;
        }
    } else {
        *bytes = bytes
            .checked_add(metadata.len())
            .context("App size overflow")?;
        ensure!(*bytes <= MAX_UNPACKED, "App bundle exceeds 256 MiB.");
    }
    Ok(())
}

fn native(program: &str, arguments: &[&str], paths: &[&Path]) -> Result<Output> {
    let output = Command::new(program)
        .args(arguments)
        .args(paths)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("Run {program}"))?;
    checked_output(program, output)
}

fn checked_output(program: &str, output: Output) -> Result<Output> {
    ensure!(
        output.status.success(),
        "{} rejected the app/archive: {}",
        program,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output)
}

fn publish_initial(extracted: &Path, applications: &Path) -> Result<()> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(extracted)?;
    let destination = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(applications)?;
    let app = CString::new(APP)?;
    // SAFETY: both descriptors refer to live directories; both names are
    // NUL-terminated single components. RENAME_EXCL atomically refuses any
    // existing destination, including one created after the initial check.
    let result = unsafe {
        libc::renameatx_np(
            source.as_raw_fd(),
            app.as_ptr(),
            destination.as_raw_fd(),
            app.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error())
            .context("Publish initial Nova.app without replacing an existing app");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
