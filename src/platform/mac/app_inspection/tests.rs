use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn evidence(bundle: &Path) -> (&'static str, Vec<String>, Vec<String>) {
    runtime_evidence(bundle, Instant::now() + crate::app_inspection::TOTAL_BUDGET)
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nova-bundle-{}-{}.app",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn runtime_uses_framework_executable_evidence_not_app_name() {
    for (framework, expected) in [
        ("Electron Framework", "electron"),
        ("Chromium Embedded Framework", "cef"),
        ("Google Chrome Framework", "chromium"),
    ] {
        let fixture = Fixture::new();
        let directory = fixture
            .0
            .join(format!("Contents/Frameworks/{framework}.framework"));
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(evidence(&fixture.0).0, "unknown");
        std::fs::write(directory.join(framework), b"fixture executable").unwrap();
        assert_eq!(evidence(&fixture.0).0, expected);
    }
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.join("Chrome Electron native misleading name"),
        b"",
    )
    .unwrap();
    assert_eq!(evidence(&fixture.0).0, "unknown");
    let versioned = fixture
        .0
        .join("Contents/Frameworks/Chromium Framework.framework/Versions/152/Chromium Framework");
    std::fs::create_dir_all(versioned.parent().unwrap()).unwrap();
    std::fs::write(versioned, b"fixture executable").unwrap();
    assert_eq!(evidence(&fixture.0).0, "chromium");
}

#[test]
fn only_exact_evidenced_profile_file_is_read_and_stale_or_symlink_rejected() {
    let fixture = Fixture::new();
    assert!(active_port(&fixture.0, 0).unwrap().is_none());
    let path = fixture.0.join("DevToolsActivePort");
    std::fs::write(&path, "43123\n/devtools/browser/current\n").unwrap();
    assert_eq!(
        active_port(&fixture.0, 0).unwrap(),
        Some((43123, "/devtools/browser/current".into()))
    );
    assert!(active_port(&fixture.0, u64::MAX).is_err());
    std::fs::write(&path, "0\n/devtools/browser/x\n").unwrap();
    assert!(active_port(&fixture.0, 0).is_err());
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/etc/hosts", &path).unwrap();
    assert!(active_port(&fixture.0, 0).is_err());
}
