use super::*;
use std::os::unix::fs::symlink;
use std::sync::OnceLock;
use zip::write::SimpleFileOptions;

fn locations(root: &Path) -> Locations {
    Locations {
        home: root.to_owned(),
        system_app: root.join("system/Applications/Nova.app"),
    }
}

fn digest(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn archive_with(root: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
    let path = root.join("test.zip");
    let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
    for (name, bytes) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default().unix_permissions(0o644))
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
    path
}

// Build and sign only a disposable, inert universal fixture; never copy,
// replace, launch or grant permissions to the user's installed Nova.app.
fn app_archive(root: &Path) -> PathBuf {
    static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes = ARCHIVE.get_or_init(|| {
        let fixture = tempfile::tempdir().unwrap();
        let app = fixture.path().join(APP);
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let source = fixture.path().join("fixture.c");
        fs::write(&source, "int main(void) { return 0; }\n").unwrap();
        let executable = macos.join("nova");
        let output = Command::new("/usr/bin/clang")
            .args(["-arch", "arm64", "-arch", "x86_64", "-o"])
            .arg(&executable)
            .arg(&source)
            .output()
            .unwrap();
        checked_output("fixture clang", output).unwrap();
        let plist = include_str!("../../../packaging/macos/Info.plist")
            .replace("@VERSION@", env!("CARGO_PKG_VERSION"))
            .replace("@SHORT_VERSION@", env!("CARGO_PKG_VERSION"))
            .replace("@BUNDLE_VERSION@", env!("CARGO_PKG_VERSION"));
        fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        for path in [&executable, &app] {
            native(
                "/usr/bin/codesign",
                &[
                    "--force",
                    "--sign",
                    "-",
                    "--timestamp=none",
                    "--identifier",
                    crate::app_service::BUNDLE_ID,
                ],
                &[path],
            )
            .unwrap();
        }
        let archive = fixture.path().join("fixture.zip");
        native(
            "/usr/bin/ditto",
            &["-c", "-k", "--sequesterRsrc", "--keepParent"],
            &[&app, &archive],
        )
        .unwrap();
        fs::read(archive).unwrap()
    });
    let path = root.join("app.zip");
    fs::write(&path, bytes).unwrap();
    path
}

fn staging_leftovers(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root.join("Applications"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".nova-app-setup-")
        })
        .collect()
}

#[test]
fn status_missing_is_passive_and_does_not_create_applications() {
    let root = tempfile::tempdir().unwrap();
    let report = execute(&AppCommand::Status, &locations(root.path())).unwrap();
    assert_eq!(report.state, SetupState::Missing);
    assert!(!root.path().join("Applications").exists());
}

#[test]
fn checksum_mismatch_preserves_archive_and_cleans_staging() {
    let root = tempfile::tempdir().unwrap();
    let path = archive_with(root.path(), &[("Nova.app/Contents/Info.plist", b"unused")]);
    let before = fs::read(&path).unwrap();
    let error = install(&path, &"0".repeat(64), &locations(root.path())).unwrap_err();
    assert!(error.to_string().contains("SHA256 mismatch"));
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(!locations(root.path()).user_app().exists());
    assert!(staging_leftovers(root.path()).is_empty());
}

#[test]
fn unsafe_paths_are_rejected_before_native_extraction() {
    for path in [
        "../escape",
        "Nova.app/../../escape",
        "/Nova.app/x",
        "Nova.app\\x",
        "Nova.app/./x",
        "Other.app/x",
        "Nova.app/a:x",
    ] {
        let root = tempfile::tempdir().unwrap();
        let archive = archive_with(root.path(), &[(path, b"bad")]);
        assert!(
            install(&archive, &digest(&archive), &locations(root.path())).is_err(),
            "{path}"
        );
        assert!(!root.path().join("escape").exists());
        assert!(!locations(root.path()).user_app().exists());
        assert!(staging_leftovers(root.path()).is_empty());
    }
}

#[test]
fn symlink_and_case_collision_archives_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("link.zip");
    let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
    writer
        .add_symlink(
            "Nova.app/Contents/MacOS/nova",
            "/usr/bin/true",
            SimpleFileOptions::default(),
        )
        .unwrap();
    writer.finish().unwrap();
    assert!(
        install(&archive, &digest(&archive), &locations(root.path()))
            .unwrap_err()
            .to_string()
            .contains("link")
    );
    let archive = archive_with(
        root.path(),
        &[
            ("Nova.app/Contents/A", b"one"),
            ("Nova.app/Contents/a", b"two"),
        ],
    );
    assert!(
        install(&archive, &digest(&archive), &locations(root.path()))
            .unwrap_err()
            .to_string()
            .contains("colliding")
    );
    assert!(staging_leftovers(root.path()).is_empty());
}

#[test]
fn file_directory_conflicts_are_rejected_in_either_order() {
    for entries in [
        [
            ("Nova.app/Contents", b"file".as_slice()),
            ("Nova.app/Contents/x", b"child".as_slice()),
        ],
        [
            ("Nova.app/Contents/x", b"child".as_slice()),
            ("Nova.app/Contents", b"file".as_slice()),
        ],
    ] {
        let root = tempfile::tempdir().unwrap();
        let archive = archive_with(root.path(), &entries);
        assert!(install(&archive, &digest(&archive), &locations(root.path())).is_err());
    }
}

#[test]
fn alternate_local_header_path_never_reaches_native_extraction() {
    let root = tempfile::tempdir().unwrap();
    let archive = archive_with(root.path(), &[("Nova.app/x", b"safe")]);
    let mut bytes = fs::read(&archive).unwrap();
    assert_eq!(&bytes[30..40], b"Nova.app/x");
    // ZIP central and local headers can disagree. The parser-selected entry
    // is serialized afresh, so ditto never sees this alternate local name.
    bytes[30..40].copy_from_slice(b"bad/../out");
    fs::write(&archive, bytes).unwrap();
    let normalized = root.path().join("normalized.zip");
    normalize_archive(File::open(&archive).unwrap(), &normalized).unwrap();
    let extraction = root.path().join("extraction");
    fs::create_dir(&extraction).unwrap();
    native("/usr/bin/ditto", &["-x", "-k"], &[&normalized, &extraction]).unwrap();
    assert_eq!(fs::read(extraction.join("Nova.app/x")).unwrap(), b"safe");
    assert!(!extraction.join("out").exists());
    assert!(!root.path().join("out").exists());
}

#[test]
fn valid_signed_fixture_is_published_and_status_keeps_development_label() {
    let root = tempfile::tempdir().unwrap();
    let archive = app_archive(root.path());
    let before = digest(&archive);
    let report = install(&archive, &before, &locations(root.path())).unwrap();
    assert_eq!(report.state, SetupState::Installed);
    assert_eq!(report.path, Some(locations(root.path()).user_app()));
    assert_eq!(report.development, Some(true));
    assert_eq!(report.signing.as_deref(), Some("ad_hoc"));
    assert_eq!(report.connector_compatible, Some(true));
    assert_eq!(digest(&archive), before);
    assert!(staging_leftovers(root.path()).is_empty());
    let status = execute(&AppCommand::Status, &locations(root.path())).unwrap();
    assert_eq!(status.state, SetupState::Installed);
    assert!(status.next_action.contains("not a notarized production"));
}

#[test]
fn existing_app_is_not_replaced_or_reopened() {
    let root = tempfile::tempdir().unwrap();
    let archive = app_archive(root.path());
    let locations = locations(root.path());
    install(&archive, &digest(&archive), &locations).unwrap();
    let binary = locations.user_app().join("Contents/MacOS/nova");
    let before = digest(&binary);
    // Existing-target check comes before opening even a nonexistent archive.
    let report = execute(
        &AppCommand::Install {
            archive: root.path().join("missing.zip"),
            sha256: "invalid".into(),
            local_sign: false,
        },
        &locations,
    )
    .unwrap();
    assert_eq!(report.state, SetupState::AlreadyInstalled);
    assert!(!report.success);
    assert_eq!(digest(&binary), before);
    assert!(staging_leftovers(root.path()).is_empty());
}

#[test]
fn existing_invalid_app_and_applications_symlinks_are_untouched() {
    let root = tempfile::tempdir().unwrap();
    let applications = root.path().join("Applications");
    fs::create_dir(&applications).unwrap();
    let target = applications.join(APP);
    fs::create_dir(&target).unwrap();
    fs::write(target.join("user-file"), "keep").unwrap();
    assert!(execute(&AppCommand::Status, &locations(root.path())).is_err());
    assert_eq!(
        fs::read_to_string(target.join("user-file")).unwrap(),
        "keep"
    );
    let other = tempfile::tempdir().unwrap();
    symlink(other.path(), root.path().join("linked")).unwrap();
    let linked = Locations {
        home: root.path().join("linked"),
        system_app: root.path().join("absent"),
    };
    // The production location resolver canonicalizes and validates the OS home;
    // an Applications link itself is always rejected by the shared path check.
    symlink(other.path(), other.path().join("Applications")).unwrap();
    assert!(execute(&AppCommand::Status, &linked).is_err());
}

#[test]
fn atomic_publication_refuses_a_target_created_after_preflight() {
    let root = tempfile::tempdir().unwrap();
    let extracted = root.path().join("extracted");
    let applications = root.path().join("Applications");
    fs::create_dir_all(extracted.join(APP)).unwrap();
    fs::create_dir_all(applications.join(APP)).unwrap();
    fs::write(applications.join(APP).join("keep"), "existing").unwrap();
    assert!(publish_initial(&extracted, &applications).is_err());
    assert_eq!(
        fs::read_to_string(applications.join(APP).join("keep")).unwrap(),
        "existing"
    );
    assert!(extracted.join(APP).exists());
}

#[test]
fn quarantine_is_retained_without_modifying_source() {
    let root = tempfile::tempdir().unwrap();
    let archive = app_archive(root.path());
    let marker = "0081;00000000;NovaSetupFixture;";
    let output = Command::new("/usr/bin/xattr")
        .args(["-w", "com.apple.quarantine", marker])
        .arg(&archive)
        .output()
        .unwrap();
    checked_output("fixture xattr", output).unwrap();
    let source_quarantine = native(
        "/usr/bin/xattr",
        &["-p", "com.apple.quarantine"],
        &[&archive],
    )
    .unwrap()
    .stdout;
    install(&archive, &digest(&archive), &locations(root.path())).unwrap();
    let unchanged = native(
        "/usr/bin/xattr",
        &["-p", "com.apple.quarantine"],
        &[&archive],
    )
    .unwrap();
    assert_eq!(
        unchanged.stdout, source_quarantine,
        "source quarantine changed"
    );
    let installed = native(
        "/usr/bin/xattr",
        &["-p", "com.apple.quarantine"],
        &[&locations(root.path()).user_app()],
    )
    .unwrap();
    // macOS/ditto may strengthen quarantine flags and rewrite attribution on
    // extraction. The download flags must remain set, never be removed.
    let text = String::from_utf8_lossy(&installed.stdout);
    let flags = u16::from_str_radix(text.split(';').next().unwrap(), 16).unwrap();
    assert_eq!(
        flags & 0x0081,
        0x0081,
        "quarantine flags were removed: {text}"
    );
}

#[test]
fn bundle_identity_and_signature_failures_do_not_publish() {
    let root = tempfile::tempdir().unwrap();
    let archive = archive_with(
        root.path(),
        &[("Nova.app/Contents/Info.plist", b"not a plist")],
    );
    assert!(install(&archive, &digest(&archive), &locations(root.path())).is_err());
    assert!(!locations(root.path()).user_app().exists());
    assert!(staging_leftovers(root.path()).is_empty());
    let archive = app_archive(root.path());
    install(&archive, &digest(&archive), &locations(root.path())).unwrap();
    let app = locations(root.path()).user_app();
    fs::write(app.join("Contents/unsealed"), "unexpected").unwrap();
    assert!(inspect_bundle(&app).is_err());
}

struct RefusingSigner(std::cell::Cell<usize>);
impl local_sign::Signer for RefusingSigner {
    fn sign(&self, app: &Path) -> Result<()> {
        self.0.set(self.0.get() + 1);
        assert!(app.to_string_lossy().contains(".nova-app-setup-"));
        assert!(app.ends_with("extracted/Nova.app"));
        anyhow::bail!("mock signing failure; manual action required")
    }
}

#[test]
fn local_sign_is_never_reached_for_hash_structure_or_original_signature_failures() {
    let root = tempfile::tempdir().unwrap();
    let signer = RefusingSigner(std::cell::Cell::new(0));
    let archive = app_archive(root.path());
    assert!(install_with_signer(
        &archive,
        &"0".repeat(64),
        &locations(root.path()),
        true,
        &signer
    )
    .is_err());
    let archive = archive_with(root.path(), &[("../unsafe", b"bad")]);
    assert!(install_with_signer(
        &archive,
        &digest(&archive),
        &locations(root.path()),
        true,
        &signer
    )
    .is_err());
    let archive = archive_with(root.path(), &[("Nova.app/Contents/Info.plist", b"invalid")]);
    assert!(install_with_signer(
        &archive,
        &digest(&archive),
        &locations(root.path()),
        true,
        &signer
    )
    .is_err());
    // Alter signed resource bytes while preserving the archive layout.
    let original = app_archive(root.path());
    let extraction = root.path().join("tampered");
    fs::create_dir(&extraction).unwrap();
    native("/usr/bin/ditto", &["-x", "-k"], &[&original, &extraction]).unwrap();
    fs::write(extraction.join("Nova.app/Contents/unsealed"), "bad").unwrap();
    let tampered = root.path().join("tampered.zip");
    native(
        "/usr/bin/ditto",
        &["-c", "-k", "--keepParent"],
        &[&extraction.join(APP), &tampered],
    )
    .unwrap();
    assert!(install_with_signer(
        &tampered,
        &digest(&tampered),
        &locations(root.path()),
        true,
        &signer
    )
    .is_err());
    // A newly signed app can have a valid outer signature while containing an
    // unsupported nested executable. Reject its layout before local signing.
    fs::remove_file(extraction.join("Nova.app/Contents/unsealed")).unwrap();
    fs::copy(
        extraction.join("Nova.app/Contents/MacOS/nova"),
        extraction.join("Nova.app/Contents/extra"),
    )
    .unwrap();
    // The copied main signature binds the app plist. Give the standalone
    // nested fixture its own valid signature before sealing the outer app.
    native(
        "/usr/bin/codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--timestamp=none",
            "--identifier",
            "com.zenith.nova.fixture",
        ],
        &[&extraction.join("Nova.app/Contents/extra")],
    )
    .unwrap();
    native(
        "/usr/bin/codesign",
        &["--force", "--sign", "-", "--timestamp=none"],
        &[&extraction.join(APP)],
    )
    .unwrap();
    let nested = root.path().join("nested.zip");
    native(
        "/usr/bin/ditto",
        &["-c", "-k", "--keepParent"],
        &[&extraction.join(APP), &nested],
    )
    .unwrap();
    let error = install_with_signer(
        &nested,
        &digest(&nested),
        &locations(root.path()),
        true,
        &signer,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("Unexpected nested executable"),
        "{error:#}"
    );
    assert_eq!(signer.0.get(), 0);
    assert!(!locations(root.path()).user_app().exists());
    assert!(staging_leftovers(root.path()).is_empty());
}

#[test]
fn local_ad_hoc_install_signs_owned_native_fixture_and_preserves_source_archive() {
    let root = tempfile::tempdir().unwrap();
    let archive = app_archive(root.path());
    native(
        "/usr/bin/xattr",
        &[
            "-w",
            "com.apple.quarantine",
            "0081;00000000;NovaSetupFixture;",
        ],
        &[&archive],
    )
    .unwrap();
    let original = digest(&archive);
    let source_quarantine = native(
        "/usr/bin/xattr",
        &["-p", "com.apple.quarantine"],
        &[&archive],
    )
    .unwrap()
    .stdout;
    let locations = locations(root.path());
    let report = install_with_signer(
        &archive,
        &original,
        &locations,
        true,
        &local_sign::NativeSigner,
    )
    .unwrap();
    assert_eq!(report.state, SetupState::Installed);
    assert!(report.success);
    assert_eq!(report.signing.as_deref(), Some("ad_hoc"));
    assert_eq!(report.development, Some(true));
    assert!(report.message.contains("local_ad_hoc"));
    assert_eq!(digest(&archive), original);
    assert_eq!(
        native(
            "/usr/bin/xattr",
            &["-p", "com.apple.quarantine"],
            &[&archive],
        )
        .unwrap()
        .stdout,
        source_quarantine
    );
    let installed = locations.user_app();
    for path in [&installed, &installed.join("Contents/MacOS/nova")] {
        let attrs = native("/usr/bin/xattr", &[], &[path]).unwrap();
        assert!(!String::from_utf8_lossy(&attrs.stdout).contains("com.apple.quarantine"));
    }
    assert_eq!(inspect_bundle(&installed).unwrap().signing, "ad_hoc");
    assert!(!root.path().join("Library").exists());
    assert!(staging_leftovers(root.path()).is_empty());
}

#[test]
fn local_sign_failure_cleans_private_stage_and_preserves_archive_and_quarantine() {
    let root = tempfile::tempdir().unwrap();
    let archive = app_archive(root.path());
    native(
        "/usr/bin/xattr",
        &[
            "-w",
            "com.apple.quarantine",
            "0081;00000000;NovaSetupFixture;",
        ],
        &[&archive],
    )
    .unwrap();
    let before = digest(&archive);
    let quarantine = native(
        "/usr/bin/xattr",
        &["-p", "com.apple.quarantine"],
        &[&archive],
    )
    .unwrap()
    .stdout;
    let signer = RefusingSigner(std::cell::Cell::new(0));
    let error =
        install_with_signer(&archive, &before, &locations(root.path()), true, &signer).unwrap_err();
    assert!(error.to_string().contains("mock signing failure"));
    assert_eq!(signer.0.get(), 1);
    assert_eq!(digest(&archive), before);
    assert_eq!(
        native(
            "/usr/bin/xattr",
            &["-p", "com.apple.quarantine"],
            &[&archive]
        )
        .unwrap()
        .stdout,
        quarantine
    );
    assert!(!locations(root.path()).user_app().exists());
    assert!(staging_leftovers(root.path()).is_empty());
}

#[test]
fn status_and_existing_app_refusal_never_access_local_signer() {
    let root = tempfile::tempdir().unwrap();
    let signer = RefusingSigner(std::cell::Cell::new(0));
    let locations = locations(root.path());
    execute_with_signer(&AppCommand::Status, &locations, &signer).unwrap();
    assert!(!root.path().join("Applications").exists());
    let archive = app_archive(root.path());
    install(&archive, &digest(&archive), &locations).unwrap();
    let report = execute_with_signer(
        &AppCommand::Install {
            archive: root.path().join("absent.zip"),
            sha256: "invalid".into(),
            local_sign: true,
        },
        &locations,
        &signer,
    )
    .unwrap();
    assert_eq!(report.state, SetupState::AlreadyInstalled);
    assert_eq!(signer.0.get(), 0);
}
