use super::*;
use std::cell::RefCell;
use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};

#[derive(Default)]
struct Mock {
    fail: Option<&'static str>,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl Runner for Mock {
    fn run(&self, program: &'static str, arguments: &[OsString]) -> Result<Output> {
        // Any future access to security/openssl or another tool fails this seam.
        assert!(matches!(program, "/usr/bin/xattr" | "/usr/bin/codesign"));
        let args: Vec<String> = arguments
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let failed = self
            .fail
            .is_some_and(|needle| args.iter().any(|arg| arg == needle));
        self.calls.borrow_mut().push((program.into(), args));
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if failed { 256 } else { 0 }),
            stdout: Vec::new(),
            stderr: if failed {
                b"mock tool failure".to_vec()
            } else {
                Vec::new()
            },
        })
    }
}

#[test]
fn local_ad_hoc_signing_targets_only_private_stage_inside_out_without_keychain() {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("private-stage/Nova.app");
    let mock = Mock::default();
    sign_with(&mock, &app).unwrap();
    let calls = mock.calls.borrow();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].0, "/usr/bin/xattr");
    assert_eq!(
        calls[0].1,
        ["-dr", "com.apple.quarantine", app.to_str().unwrap()]
    );
    for (sign, verify, target) in [(1, 2, app.join("Contents/MacOS/nova")), (3, 4, app.clone())] {
        assert_eq!(calls[sign].0, "/usr/bin/codesign");
        assert!(calls[sign].1.windows(2).any(|a| a == ["--sign", "-"]));
        assert!(calls[sign]
            .1
            .windows(2)
            .any(|a| a == ["--identifier", "com.zenith.nova"]));
        assert_eq!(calls[sign].1.last().unwrap(), target.to_str().unwrap());
        assert_eq!(
            calls[verify].1,
            ["--verify", "--deep", "--strict", target.to_str().unwrap()]
        );
    }
    assert!(calls.iter().all(|(_, args)| !args
        .iter()
        .any(|arg| arg == "--keychain" || arg == "--requirements")));
}

#[test]
fn quarantine_or_signing_or_verification_failure_stops_before_next_step() {
    let root = tempfile::tempdir().unwrap();
    for (failure, expected_calls) in [("-dr", 1), ("--force", 2), ("--verify", 3)] {
        let mock = Mock {
            fail: Some(failure),
            ..Mock::default()
        };
        assert!(sign_with(&mock, &root.path().join("Nova.app")).is_err());
        assert_eq!(mock.calls.borrow().len(), expected_calls);
    }
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
