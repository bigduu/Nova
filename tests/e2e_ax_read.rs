//! Live, opt-in smoke test for the screenshot-free `ax:read` platform seam.
//!
//! Run against the focused app:
//!
//!     cargo test --test e2e_ax_read -- --ignored --nocapture
//!
//! Or target a known native/browser/Electron app without focusing it first:
//!
//!     NOVA_AX_WINDOW=Finder cargo test --test e2e_ax_read -- --ignored --nocapture
//!     NOVA_AX_WINDOW=Chrome cargo test --test e2e_ax_read -- --ignored --nocapture

#![cfg(any(target_os = "macos", target_os = "windows"))]

use nova::platform::{UiNodeValue, UiReadCoverage, UiReadMode, UiSnapshotOptions};
use std::time::{Duration, Instant};

#[test]
#[ignore = "requires a logged-in desktop and Accessibility/UIA provider"]
fn semantic_snapshot_reads_a_live_app_without_pixel_capture() {
    let query = std::env::var("NOVA_AX_WINDOW")
        .ok()
        .filter(|query| !query.trim().is_empty());
    let deadline = Instant::now() + Duration::from_secs(20);
    let target = nova::platform::ui_tree()
        .resolve_target(query.as_deref(), None, deadline)
        .expect("resolve AX/UIA target without screenshot metadata");
    let snapshot = nova::platform::ui_tree()
        .read_snapshot(
            &target,
            UiSnapshotOptions {
                mode: UiReadMode::All,
                max_nodes: 400,
                max_chars: 50_000,
                deadline,
            },
        )
        .expect("read semantic snapshot");

    assert_eq!(snapshot.target.pid, target.pid);
    assert!(matches!(
        snapshot.coverage,
        UiReadCoverage::Complete | UiReadCoverage::Partial | UiReadCoverage::Empty
    ));
    for collected in &snapshot.nodes {
        if collected.node.role.to_lowercase().contains("password")
            || collected.node.role.to_lowercase().contains("secure")
        {
            assert_eq!(
                collected.node.value,
                UiNodeValue::Redacted,
                "secure controls must not cross the platform seam as text"
            );
        }
    }
    eprintln!(
        "ax:read target={} {:?} nodes={} coverage={}",
        snapshot.target.app_name,
        snapshot.target.window_title,
        snapshot.nodes.len(),
        snapshot.coverage.as_str()
    );
}
