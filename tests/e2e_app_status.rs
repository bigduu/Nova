//! Real local listener/status contract without Nova's desktop bootstrap, menu,
//! capture helper, system permissions, or any installed application.
#![cfg(unix)]

use nova::app_status::{
    AppStatus, Controller, Permission, PermissionState, Permissions, ServiceState,
};
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct DeniedPermissions;
impl Permissions for DeniedPermissions {
    fn check(&self, _: Permission) -> bool {
        false
    }
    fn request(&self, _: Permission) {
        panic!("service/refresh must not request permission");
    }
    fn open_settings(&self, _: Permission) -> Result<(), String> {
        panic!("service/refresh must not open Settings");
    }
}

struct Fixture {
    directory: PathBuf,
    app_socket: Option<OsString>,
    chrome_socket: Option<OsString>,
}

impl Fixture {
    fn new() -> Self {
        let directory = PathBuf::from(format!("/tmp/nova-status-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let fixture = Self {
            directory,
            app_socket: std::env::var_os("NOVA_APP_SOCKET"),
            chrome_socket: std::env::var_os("NOVA_CHROME_SOCKET"),
        };
        // This integration test is its own process with a single test. These
        // overrides never address or launch the user's installed app service.
        std::env::set_var("NOVA_APP_SOCKET", fixture.directory.join("service.sock"));
        std::env::set_var("NOVA_CHROME_SOCKET", fixture.directory.join("chrome.sock"));
        fixture
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in [
            ("NOVA_APP_SOCKET", &self.app_socket),
            ("NOVA_CHROME_SOCKET", &self.chrome_socket),
        ] {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_file(self.directory.join("service.sock"));
        let _ = std::fs::remove_file(self.directory.join("service.lock"));
        let _ = std::fs::remove_file(self.directory.join("chrome.sock"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[tokio::test]
async fn denied_permissions_do_not_block_ready_handshake_or_duplicate_detection() {
    let fixture = Fixture::new();
    let status = AppStatus::default();
    let controller = Controller::new(status.clone(), DeniedPermissions);
    assert_eq!(status.snapshot().service, ServiceState::Starting);
    let service = tokio::spawn(nova::app_service::run_with_status(status.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while status.snapshot().service != ServiceState::Ready {
            assert!(
                !service.is_finished(),
                "fixture service failed to become ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        fixture.directory.join("chrome.sock").exists(),
        "ready follows both listener binds"
    );
    controller.refresh();
    assert_eq!(status.snapshot().accessibility, PermissionState::NotGranted);
    assert_eq!(
        status.snapshot().screen_recording,
        PermissionState::NotGranted
    );

    let duplicate = AppStatus::default();
    assert_eq!(
        nova::app_service::run_with_status(duplicate.clone())
            .await
            .unwrap(),
        nova::app_service::ServiceExit::AlreadyRunning
    );
    assert_ne!(duplicate.snapshot().service, ServiceState::Failed);
    assert_eq!(status.snapshot().service, ServiceState::Ready);

    let stream = tokio::net::UnixStream::connect(fixture.directory.join("service.sock"))
        .await
        .unwrap();
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"status-fixture\",\"version\":\"1\"}}}\n").await.unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), read.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(response["result"]["serverInfo"].is_object());
    write.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n").await.unwrap();
    line.clear();
    tokio::time::timeout(Duration::from_secs(5), read.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"], serde_json::json!({}));
    drop((read, write));
    service.abort();
    assert!(service.await.unwrap_err().is_cancelled());
    assert!(
        !fixture.directory.join("service.sock").exists(),
        "owned socket is released on Quit/cancellation"
    );

    std::env::set_var("NOVA_APP_SOCKET", "invalid-relative-fixture");
    let failed = AppStatus::default();
    assert!(nova::app_service::run_with_status(failed.clone())
        .await
        .is_err());
    assert_eq!(failed.snapshot().service, ServiceState::Failed);
    Controller::new(failed.clone(), DeniedPermissions).refresh();
    assert_eq!(
        failed.snapshot().service,
        ServiceState::Failed,
        "permission refresh must retain service failure"
    );
}
