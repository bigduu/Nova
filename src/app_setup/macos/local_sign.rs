//! Explicit local signing of an already verified, private installation stage.
//! Never select by CN alone, change trust/ACL policy, or replace an identity.

use super::{checked_output, validate_owned_directory};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Output, Stdio};

pub(super) const IDENTITY_NAME: &str = "Zenith Nova Code Signing";

pub(super) trait Signer {
    fn sign(&self, app: &Path, home: &Path) -> Result<()>;
}

pub(super) struct NativeSigner;

impl Signer for NativeSigner {
    fn sign(&self, app: &Path, home: &Path) -> Result<()> {
        let keychains = home.join("Library/Keychains");
        validate_owned_directory(&home.join("Library"))?;
        validate_owned_directory(&keychains)?;
        let keychain = keychains.join("login.keychain-db");
        validate_owned_file(&keychain)?;
        // Keep a single owned lock inode; unlinking it would allow a third
        // installer to acquire a different inode while another still waits.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(keychains.join(".nova-local-sign.lock"))?;
        let metadata = lock.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.nlink() == 1
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "Refusing unsafe local signing lock."
        );
        // SAFETY: lock is a live descriptor. A nonblocking lock gives a useful
        // retry report instead of leaving setup blocked behind another prompt.
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another Nova local-sign operation is active; wait and retry."
        );
        let private = tempfile::Builder::new()
            .prefix(".nova-sign-")
            .tempdir_in(app.parent().context("Missing stage parent")?)?;
        sign_with(&NativeRunner, app, &keychain, private.path())
        // Closing lock releases flock, including all error paths.
    }
}

fn validate_owned_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .context("Inspect login keychain; configure it manually if unavailable")?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0,
        "Login keychain must be a regular file owned by this account and not writable by others."
    );
    Ok(())
}

// This release's app contains only Contents/MacOS/nova. Fail closed on a new
// nested-code layout instead of adding an unreviewed generic bundle signer.
pub(super) fn validate_code_layout(app: &Path) -> Result<()> {
    fn walk(path: &Path, main: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                walk(&entry?.path(), main)?;
            }
        } else if path != main {
            let mut magic = [0_u8; 4];
            let count = File::open(path)?.read(&mut magic)?;
            let mach_o = count == 4
                && matches!(
                    u32::from_be_bytes(magic),
                    0xfeedface
                        | 0xcefaedfe
                        | 0xfeedfacf
                        | 0xcffaedfe
                        | 0xcafebabe
                        | 0xbebafeca
                        | 0xcafebabf
                        | 0xbfbafeca
                );
            ensure!(!mach_o && metadata.mode() & 0o111 == 0,
                "Unexpected nested executable; this local-sign installer supports only Contents/MacOS/nova.");
        }
        Ok(())
    }
    walk(app, &app.join("Contents/MacOS/nova"))
}

trait Runner {
    fn run(&self, program: &'static str, arguments: &[OsString]) -> Result<Output>;
}

struct NativeRunner;
impl Runner for NativeRunner {
    fn run(&self, program: &'static str, arguments: &[OsString]) -> Result<Output> {
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .env("LC_ALL", "C")
            .env_remove("OPENSSL_CONF")
            .env_remove("OPENSSL_MODULES")
            .env_remove("OPENSSL_ENGINES")
            .output()
            .with_context(|| {
                format!("Run {program}; unlock/configure the login keychain manually if needed")
            })
    }
}

fn run(runner: &dyn Runner, program: &'static str, arguments: &[&OsStr]) -> Result<Output> {
    let output = runner.run(
        program,
        &arguments
            .iter()
            .map(|s| s.to_os_string())
            .collect::<Vec<_>>(),
    )?;
    checked_output(program, output)
}

fn query(runner: &dyn Runner, arguments: &[&OsStr]) -> Result<String> {
    let output = run(runner, "/usr/bin/security", arguments)?;
    // Some SecurityTool enumeration errors return zero; errors must never be
    // interpreted as an empty keychain and trigger another identity creation.
    ensure!(
        output.stderr.iter().all(u8::is_ascii_whitespace),
        "Keychain enumeration reported an error; inspect it manually."
    );
    ensure!(
        output.stdout.len() <= 2 * 1024 * 1024,
        "Unexpected keychain response size."
    );
    String::from_utf8(output.stdout).context("Invalid keychain response")
}

fn fingerprint(value: &str) -> Result<String> {
    let value = value.replace(':', "");
    ensure!(
        value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Malformed certificate fingerprint."
    );
    Ok(value.to_ascii_uppercase())
}

fn pem_certificate(text: &str) -> Result<Option<&str>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    ensure!(
        text.starts_with(BEGIN)
            && text.ends_with(END)
            && text.matches(BEGIN).count() == 1
            && text.matches(END).count() == 1,
        "Ambiguous or malformed signing certificates; inspect the login keychain manually."
    );
    Ok(Some(text))
}

fn certificate_hash(runner: &dyn Runner, pem: &str, scratch: &Path) -> Result<String> {
    let file = scratch.join("selected.pem");
    fs::write(&file, pem)?;
    let output = run(
        runner,
        "/usr/bin/openssl",
        &[
            "x509".as_ref(),
            "-in".as_ref(),
            file.as_os_str(),
            "-noout".as_ref(),
            "-subject".as_ref(),
            "-issuer".as_ref(),
            "-nameopt".as_ref(),
            "RFC2253".as_ref(),
            "-fingerprint".as_ref(),
            "-sha1".as_ref(),
            "-text".as_ref(),
        ],
    )?;
    let text = String::from_utf8(output.stdout)?;
    let expected = format!("CN={IDENTITY_NAME}");
    let field = |prefix: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .map(str::trim)
    };
    ensure!(
        field("subject=") == Some(expected.as_str()) && field("issuer=") == Some(expected.as_str()),
        "Existing signing certificate does not have the exact self-signed Nova CN."
    );
    ensure!(
        text.contains("X509v3 Extended Key Usage:")
            && text.lines().any(|line| line.trim() == "Code Signing"),
        "Existing Nova certificate lacks the code-signing purpose."
    );
    let hash = fingerprint(field("SHA1 Fingerprint=").context("Missing certificate fingerprint")?)?;
    // Evaluate with this certificate as an explicit local anchor, no search
    // list/network access and no persistent trust change. LibreSSL's CA-chain
    // verifier rejects the existing CA:false signing certificate as an anchor.
    run(
        runner,
        "/usr/bin/security",
        &[
            "verify-cert".as_ref(),
            "-c".as_ref(),
            file.as_os_str(),
            "-r".as_ref(),
            file.as_os_str(),
            "-p".as_ref(),
            "codeSign".as_ref(),
            "-N".as_ref(),
            "-L".as_ref(),
            "-q".as_ref(),
        ],
    )?;
    Ok(hash)
}

#[derive(Debug)]
struct Identity {
    hash: String,
    label: String,
    status: String,
}

fn identity_records(text: &str) -> Result<Vec<Identity>> {
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    ensure!(
        lines.first() == Some(&"Policy: Code Signing")
            && lines.get(1) == Some(&"Matching identities"),
        "Incomplete identity enumeration."
    );
    let mut index = 2;
    let mut records = Vec::new();
    while let Some(line) = lines.get(index) {
        if line.ends_with(" identities found") {
            break;
        }
        let (number, record) = line.split_once(") ").context("Malformed identity record")?;
        ensure!(
            number.parse::<usize>()? == records.len() + 1,
            "Unexpected identity sequence."
        );
        let (hash, record) = record.split_once(' ').context("Missing identity label")?;
        let record = record
            .strip_prefix('"')
            .context("Malformed identity label")?;
        let (label, status) = record
            .split_once('"')
            .context("Unterminated identity label")?;
        let status = status.trim();
        ensure!(
            status.is_empty() || (status.starts_with("(CSSMERR_") && status.ends_with(')')),
            "Malformed identity status."
        );
        records.push(Identity {
            hash: fingerprint(hash)?,
            label: label.into(),
            status: status.into(),
        });
        index += 1;
    }
    ensure!(
        lines.get(index) == Some(&format!("{} identities found", records.len()).as_str())
            && lines.get(index + 1) == Some(&"Valid identities only"),
        "Incomplete identity count."
    );
    // The second section intentionally repeats policy-valid records. Validate
    // its shape/count but use only the first section for identity selection.
    let valid_count = lines
        .last()
        .and_then(|line| line.strip_suffix(" valid identities found"))
        .context("Missing valid identity count")?
        .parse::<usize>()?;
    ensure!(
        lines.len() == index + 3 + valid_count,
        "Incomplete valid identity section."
    );
    let mut valid_hashes = BTreeSet::new();
    for (offset, line) in lines[index + 2..lines.len() - 1].iter().enumerate() {
        let (number, record) = line
            .split_once(") ")
            .context("Malformed valid identity record")?;
        ensure!(
            number.parse::<usize>()? == offset + 1,
            "Unexpected valid identity sequence."
        );
        let (hash, _) = record
            .split_once(' ')
            .context("Malformed valid identity hash")?;
        let hash = fingerprint(hash)?;
        ensure!(
            records.iter().any(|entry| entry.hash == hash) && valid_hashes.insert(hash),
            "Inconsistent valid identity section."
        );
    }
    Ok(records)
}

fn select_identity(runner: &dyn Runner, keychain: &Path, scratch: &Path) -> Result<Option<String>> {
    let certificates = query(
        runner,
        &[
            "find-certificate".as_ref(),
            "-a".as_ref(),
            "-c".as_ref(),
            IDENTITY_NAME.as_ref(),
            "-p".as_ref(),
            keychain.as_os_str(),
        ],
    )?;
    let identities = identity_records(&query(
        runner,
        &[
            "find-identity".as_ref(),
            "-p".as_ref(),
            "codesigning".as_ref(),
            keychain.as_os_str(),
        ],
    )?)?;
    match pem_certificate(&certificates)? {
        None => {
            ensure!(
                !identities
                    .iter()
                    .any(|entry| entry.label.contains(IDENTITY_NAME)),
                "Existing Nova identity has no matching certificate; inspect it manually."
            );
            Ok(None)
        }
        Some(pem) => {
            let hash = certificate_hash(runner, pem, scratch)?;
            let matching: Vec<_> = identities
                .iter()
                .filter(|entry| entry.hash == hash || entry.label.contains(IDENTITY_NAME))
                .collect();
            ensure!(matching.len() == 1 && matching[0].hash == hash,
                "Nova certificate has no unique matching private-key identity; inspect it manually.");
            ensure!(
                matching[0].status.is_empty() || matching[0].status == "(CSSMERR_TP_NOT_TRUSTED)",
                "Nova signing identity is unusable: {}; inspect it manually.",
                matching[0].status
            );
            Ok(Some(hash))
        }
    }
}

fn create_identity(runner: &dyn Runner, keychain: &Path, scratch: &Path) -> Result<String> {
    let config = scratch.join("request.cfg");
    let key = scratch.join("private.pem");
    let cert = scratch.join("created.pem");
    fs::write(&config, format!("[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n[dn]\nCN={IDENTITY_NAME}\n[v3]\nbasicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n"))?;
    run(
        runner,
        "/usr/bin/openssl",
        &[
            "req".as_ref(),
            "-x509".as_ref(),
            "-newkey".as_ref(),
            "rsa:2048".as_ref(),
            "-nodes".as_ref(),
            "-sha256".as_ref(),
            "-days".as_ref(),
            "3650".as_ref(),
            "-keyout".as_ref(),
            key.as_os_str(),
            "-out".as_ref(),
            cert.as_os_str(),
            "-config".as_ref(),
            config.as_os_str(),
        ],
    )?;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    let created_pem = fs::read_to_string(&cert)?;
    let created_hash = certificate_hash(
        runner,
        pem_certificate(&created_pem)?.context("Missing newly generated certificate")?,
        scratch,
    )?;
    // Certificate first: a later private-key failure leaves a visible
    // certificate-only state that future installs reject instead of recreating.
    for file in [&cert, &key] {
        run(runner, "/usr/bin/security", &["import".as_ref(), file.as_os_str(), "-k".as_ref(), keychain.as_os_str(), "-T".as_ref(), "/usr/bin/codesign".as_ref()])
            .context("Local identity creation failed and may be partial; inspect the login keychain manually before retrying")?;
    }
    Ok(created_hash)
}

fn requirement(hash: &str) -> String {
    format!(
        "identifier \"{}\" and certificate leaf = H\"{hash}\"",
        crate::app_service::BUNDLE_ID
    )
}

fn stable_requirement_hash(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim().trim_start_matches("# ");
        let value = line.strip_prefix("designated => ")?;
        let prefix = format!(
            "identifier \"{}\" and certificate leaf = H\"",
            crate::app_service::BUNDLE_ID
        );
        fingerprint(value.strip_prefix(&prefix)?.strip_suffix('"')?).ok()
    })
}

pub(super) fn has_stable_requirement(text: &str) -> bool {
    stable_requirement_hash(text).is_some()
}

fn sign_with(runner: &dyn Runner, app: &Path, keychain: &Path, scratch: &Path) -> Result<()> {
    let hash = match select_identity(runner, keychain, scratch)? {
        Some(hash) => hash,
        None => {
            let created = create_identity(runner, keychain, scratch)?;
            let selected = select_identity(runner, keychain, scratch)?.context("Created identity is unavailable; inspect the keychain manually; do not automatically recreate it")?;
            ensure!(
                selected == created,
                "New signing identity changed during creation; inspect the keychain manually."
            );
            selected
        }
    };
    let predicate = requirement(&hash);
    let designated = format!("=designated => {predicate}");
    let test_requirement = format!("={predicate}");
    run(
        runner,
        "/usr/bin/xattr",
        &[
            "-dr".as_ref(),
            "com.apple.quarantine".as_ref(),
            app.as_os_str(),
        ],
    )?;
    for path in [app.join("Contents/MacOS/nova"), app.to_path_buf()] {
        run(
            runner,
            "/usr/bin/codesign",
            &[
                "--force".as_ref(),
                "--timestamp=none".as_ref(),
                "--identifier".as_ref(),
                crate::app_service::BUNDLE_ID.as_ref(),
                "--keychain".as_ref(),
                keychain.as_os_str(),
                "--sign".as_ref(),
                hash.as_ref(),
                "--requirements".as_ref(),
                designated.as_ref(),
                path.as_os_str(),
            ],
        )?;
        run(
            runner,
            "/usr/bin/codesign",
            &[
                "--verify".as_ref(),
                "--deep".as_ref(),
                "--strict".as_ref(),
                "-R".as_ref(),
                test_requirement.as_ref(),
                path.as_os_str(),
            ],
        )?;
        let output = run(
            runner,
            "/usr/bin/codesign",
            &[
                "--display".as_ref(),
                "--requirements".as_ref(),
                "-".as_ref(),
                path.as_os_str(),
            ],
        )?;
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        ensure!(
            stable_requirement_hash(&text).as_deref() == Some(hash.as_str()),
            "Local signature has an unexpected designated requirement."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
