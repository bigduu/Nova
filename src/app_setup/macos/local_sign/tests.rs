use super::*;
use std::cell::{Cell, RefCell};
use std::os::unix::{fs::symlink, process::ExitStatusExt};

const HASH: &str = "0123456789ABCDEF0123456789ABCDEF01234567";
const PEM: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";

fn identities(records: &[(String, &str, &str)], valid: bool) -> String {
    let mut text = "\nPolicy: Code Signing\n  Matching identities\n".to_string();
    for (index, (hash, label, status)) in records.iter().enumerate() {
        text += &format!("  {}) {hash} \"{label}\" {status}\n", index + 1);
    }
    text += &format!(
        "     {} identities found\n\n  Valid identities only\n",
        records.len()
    );
    if valid {
        for (index, (hash, label, _)) in records.iter().enumerate() {
            text += &format!("  {}) {hash} \"{label}\"\n", index + 1);
        }
    }
    text + &format!(
        "     {} valid identities found\n",
        if valid { records.len() } else { 0 }
    )
}

struct Mock {
    certificate: Cell<bool>,
    private_key: Cell<bool>,
    certificates_override: Option<String>,
    identities_override: Option<String>,
    subject: String,
    fail: Option<String>,
    query_warning: bool,
    wrong_requirement: bool,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl Mock {
    fn existing() -> Self {
        Self {
            certificate: Cell::new(true),
            private_key: Cell::new(true),
            certificates_override: None,
            identities_override: None,
            subject: IDENTITY_NAME.into(),
            fail: None,
            query_warning: false,
            wrong_requirement: false,
            calls: RefCell::new(Vec::new()),
        }
    }
    fn missing() -> Self {
        let mock = Self::existing();
        mock.certificate.set(false);
        mock.private_key.set(false);
        mock
    }
    fn mutations(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .filter(|(_, a)| {
                a.first()
                    .is_some_and(|s| s == "import" || s == "req" || s == "-dr" || s == "--force")
            })
            .map(|(_, a)| a.join(" "))
            .collect()
    }
}

impl Runner for Mock {
    fn run(&self, program: &'static str, arguments: &[OsString]) -> Result<Output> {
        let args: Vec<String> = arguments
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        self.calls.borrow_mut().push((program.into(), args.clone()));
        let mut stdout = String::new();
        let mut stderr = String::new();
        let failed = self
            .fail
            .as_ref()
            .is_some_and(|needle| args.join(" ").contains(needle));
        if failed {
            stderr = "mock tool failure".into();
        } else {
            match (program, args[0].as_str()) {
            ("/usr/bin/security", "find-certificate") => {
                stdout = self.certificates_override.clone().unwrap_or_else(|| if self.certificate.get() { PEM.into() } else { String::new() });
                if self.query_warning { stderr = "SecKeychainSearchCopyNext: unexpected error".into(); }
            }
            ("/usr/bin/security", "find-identity") => {
                stdout = self.identities_override.clone().unwrap_or_else(|| identities(&if self.private_key.get() { vec![(HASH.into(), IDENTITY_NAME, "(CSSMERR_TP_NOT_TRUSTED)")] } else { vec![] }, false));
            }
            ("/usr/bin/openssl", "x509") => stdout = format!("subject=CN={}\nissuer=CN={}\nSHA1 Fingerprint={}\n            X509v3 Extended Key Usage:\n                Code Signing\n", self.subject, self.subject, HASH),
            ("/usr/bin/security", "verify-cert") => stdout = "certificate: OK\n".into(),
            ("/usr/bin/openssl", "req") => {
                for (flag, contents) in [("-keyout", "PRIVATE TEST FIXTURE"), ("-out", PEM)] {
                    let index = args.iter().position(|a| a == flag).unwrap();
                    fs::write(&args[index + 1], contents)?;
                }
            }
            ("/usr/bin/security", "import") => {
                if args[1].ends_with("created.pem") { self.certificate.set(true); }
                else if args[1].ends_with("private.pem") { self.private_key.set(true); }
                else { panic!("unexpected import"); }
            }
            ("/usr/bin/codesign", "--display") => {
                stdout = format!("designated => {}\n", requirement(&if self.wrong_requirement { "A".repeat(40) } else { HASH.to_ascii_lowercase() }));
            }
            ("/usr/bin/codesign", "--verify") => {
                let at = args.iter().position(|arg| arg == "-R").unwrap();
                assert!(args[at + 1].starts_with('='), "codesign otherwise treats it as a filename");
            }
            ("/usr/bin/codesign", "--force") | ("/usr/bin/xattr", "-dr") => {}
            _ => panic!("Unexpected native command: {program} {args:?}"),
        }
        }
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if failed { 256 } else { 0 }),
            stdout: stdout.into_bytes(),
            stderr: stderr.into_bytes(),
        })
    }
}

fn attempt(mock: &Mock) -> Result<()> {
    let root = tempfile::tempdir()?;
    sign_with(
        mock,
        &root.path().join("private-stage/Nova.app"),
        &root.path().join("login.keychain-db"),
        root.path(),
    )
}

#[test]
fn unique_untrusted_identity_is_reused_with_stable_inside_out_signatures() {
    let mock = Mock::existing();
    attempt(&mock).unwrap();
    let calls = mock.calls.borrow();
    let signed: Vec<_> = calls
        .iter()
        .filter(|(_, args)| args[0] == "--force")
        .collect();
    assert_eq!(signed.len(), 2);
    assert!(signed[0]
        .1
        .last()
        .unwrap()
        .ends_with("Nova.app/Contents/MacOS/nova"));
    assert!(signed[1].1.last().unwrap().ends_with("Nova.app"));
    for (_, args) in signed {
        assert!(args.windows(2).any(|a| a == ["--sign", HASH]));
        assert!(args
            .windows(2)
            .any(|a| a == ["--identifier", "com.zenith.nova"]));
        assert!(args
            .iter()
            .any(|a| a == &format!("=designated => {}", requirement(HASH))));
    }
    assert!(calls
        .iter()
        .all(|(_, args)| !args.iter().any(|arg| arg == "-v"
            || arg == "set-key-partition-list"
            || arg == "delete-certificate"
            || arg == "add-trusted-cert")));
    let changed = mock.mutations();
    assert_eq!(changed.len(), 3);
    assert!(changed[0].starts_with("-dr com.apple.quarantine "));
    assert!(changed[0].contains("private-stage/Nova.app"));
}

#[test]
fn missing_identity_is_created_certificate_first_and_reenumerated() {
    let mock = Mock::missing();
    attempt(&mock).unwrap();
    let changed = mock.mutations();
    assert!(changed[0].starts_with("req -x509 "));
    assert!(changed[1].contains("created.pem -k "));
    assert!(changed[2].contains("private.pem -k "));
    assert_eq!(
        mock.calls
            .borrow()
            .iter()
            .filter(|(_, a)| a[0] == "find-identity")
            .count(),
        2
    );
    assert!(changed[1..3]
        .iter()
        .all(|call| call.ends_with("-T /usr/bin/codesign")));
}

#[test]
fn certificate_only_state_is_rejected_without_recreation() {
    let mock = Mock::existing();
    mock.private_key.set(false);
    assert!(attempt(&mock)
        .unwrap_err()
        .to_string()
        .contains("unique matching"));
    assert!(mock.mutations().is_empty());
}

#[test]
fn identity_without_matching_certificate_is_rejected_without_creation() {
    let mock = Mock::existing();
    mock.certificate.set(false);
    assert!(attempt(&mock)
        .unwrap_err()
        .to_string()
        .contains("no matching certificate"));
    assert!(mock.mutations().is_empty());
}

#[test]
fn duplicate_certificates_or_private_key_identities_fail_closed() {
    let mut mock = Mock::existing();
    mock.certificates_override = Some(format!("{PEM}\n{PEM}"));
    assert!(attempt(&mock).is_err());
    assert!(mock.mutations().is_empty());
    mock.certificates_override = None;
    mock.identities_override = Some(identities(
        &[
            (HASH.into(), IDENTITY_NAME, ""),
            ("A".repeat(40), IDENTITY_NAME, ""),
        ],
        false,
    ));
    assert!(attempt(&mock).is_err());
    assert!(mock.mutations().is_empty());
}

#[test]
fn cn_substring_match_and_unusable_identity_are_not_accepted() {
    let mut mock = Mock::existing();
    mock.subject += " Unrelated";
    assert!(attempt(&mock)
        .unwrap_err()
        .to_string()
        .contains("exact self-signed"));
    assert!(mock.mutations().is_empty());
    mock.subject = IDENTITY_NAME.into();
    mock.identities_override = Some(identities(
        &[(HASH.into(), IDENTITY_NAME, "(CSSMERR_TP_CERT_EXPIRED)")],
        false,
    ));
    assert!(attempt(&mock).unwrap_err().to_string().contains("unusable"));
    assert!(mock.mutations().is_empty());
}

#[test]
fn valid_identity_section_repeats_do_not_create_ambiguity() {
    let mut mock = Mock::existing();
    mock.identities_override = Some(identities(&[(HASH.into(), IDENTITY_NAME, "")], true));
    attempt(&mock).unwrap();
    assert_eq!(mock.mutations().len(), 3);
}

#[test]
fn zero_exit_with_enumeration_error_never_creates_identity() {
    let mut mock = Mock::missing();
    mock.query_warning = true;
    assert!(attempt(&mock)
        .unwrap_err()
        .to_string()
        .contains("enumeration reported"));
    assert!(mock.mutations().is_empty());
}

#[test]
fn truncated_or_malformed_identity_enumeration_never_creates_identity() {
    for response in ["", "Policy: Code Signing\nMatching identities\n0 identities found", "Policy: Code Signing\nMatching identities\n1 identities found\nValid identities only\n0 valid identities found"] {
        let mut mock = Mock::missing();
        mock.identities_override = Some(response.into());
        assert!(attempt(&mock).is_err());
        assert!(mock.mutations().is_empty());
    }
}

#[test]
fn expired_or_invalid_certificate_never_reaches_mutation() {
    let mut mock = Mock::existing();
    mock.fail = Some("verify-cert".into());
    assert!(attempt(&mock).is_err());
    assert!(mock.mutations().is_empty());
}

#[test]
fn partial_import_has_manual_error_and_next_attempt_refuses_recreation() {
    let mut mock = Mock::missing();
    mock.fail = Some("private.pem -k".into());
    assert!(format!("{:#}", attempt(&mock).unwrap_err()).contains("may be partial"));
    assert!(mock.certificate.get());
    assert!(!mock.private_key.get());
    mock.fail = None;
    assert!(attempt(&mock)
        .unwrap_err()
        .to_string()
        .contains("unique matching"));
    assert_eq!(
        mock.calls
            .borrow()
            .iter()
            .filter(|(_, a)| a[0] == "req")
            .count(),
        1
    );
}

#[test]
fn signing_and_requirement_failure_are_reported() {
    let mut mock = Mock::existing();
    mock.fail = Some("--force".into());
    assert!(attempt(&mock).is_err());
    assert!(!mock.calls.borrow().iter().any(|(_, a)| a[0] == "--verify"));
    mock.fail = None;
    mock.wrong_requirement = true;
    assert!(attempt(&mock)
        .unwrap_err()
        .to_string()
        .contains("unexpected designated"));
}

#[test]
fn stable_requirement_requires_fixed_identifier_and_exact_certificate_binding() {
    assert!(has_stable_requirement(&format!(
        "# designated => {}\n",
        requirement(HASH)
    )));
    assert!(!has_stable_requirement(
        "designated => identifier \"com.zenith.nova\""
    ));
    assert!(!has_stable_requirement(&format!(
        "designated => {} or true",
        requirement(HASH)
    )));
    assert!(!has_stable_requirement(&format!(
        "designated => {}",
        requirement(HASH).replace("com.zenith.nova", "other")
    )));
}

#[test]
fn unexpected_nested_mach_o_or_executable_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("Nova.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(app.join("Contents/MacOS/nova"), [0xcf, 0xfa, 0xed, 0xfe]).unwrap();
    validate_code_layout(&app).unwrap();
    let nested = app.join("Contents/extra");
    fs::write(&nested, [0xca, 0xfe, 0xba, 0xbe]).unwrap();
    assert!(validate_code_layout(&app).is_err());
    fs::write(&nested, "#!/bin/sh").unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(validate_code_layout(&app).is_err());
}

#[test]
fn unsafe_keychain_and_lock_are_rejected_before_native_commands() {
    let root = tempfile::tempdir().unwrap();
    let keychains = root.path().join("Library/Keychains");
    fs::create_dir_all(&keychains).unwrap();
    let keychain = keychains.join("login.keychain-db");
    fs::write(&keychain, "owned fixture, not a real keychain").unwrap();
    fs::set_permissions(&keychain, fs::Permissions::from_mode(0o600)).unwrap();
    let lock = keychains.join(".nova-local-sign.lock");
    symlink(&keychain, &lock).unwrap();
    assert!(NativeSigner
        .sign(&root.path().join("Nova.app"), root.path())
        .is_err());
    assert_eq!(
        fs::read_to_string(&keychain).unwrap(),
        "owned fixture, not a real keychain"
    );
    fs::remove_file(&lock).unwrap();
    fs::set_permissions(&keychain, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(NativeSigner
        .sign(&root.path().join("Nova.app"), root.path())
        .is_err());
}

#[test]
fn concurrent_local_sign_operation_fails_without_keychain_access() {
    let root = tempfile::tempdir().unwrap();
    let keychains = root.path().join("Library/Keychains");
    fs::create_dir_all(&keychains).unwrap();
    fs::write(keychains.join("login.keychain-db"), "owned fixture").unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(keychains.join(".nova-local-sign.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let error = NativeSigner
        .sign(&root.path().join("Nova.app"), root.path())
        .unwrap_err();
    assert!(error.to_string().contains("Another Nova"));
}

#[test]
fn requirement_compiles_and_round_trips_with_native_tool_without_signing() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("requirement.bin");
    let predicate = format!("={}", requirement(HASH));
    run(
        &NativeRunner,
        "/usr/bin/csreq",
        &[
            "-r".as_ref(),
            predicate.as_ref(),
            "-b".as_ref(),
            file.as_os_str(),
        ],
    )
    .unwrap();
    let output = run(
        &NativeRunner,
        "/usr/bin/csreq",
        &["-r".as_ref(), file.as_os_str(), "-t".as_ref()],
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        requirement(&HASH.to_ascii_lowercase())
    );
}
