//! Process-exit regression for the same final runtime boundary used by the
//! menu. No production CLI flag, UI, permission API, or user application is used.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--test-owned-process") {
        return mac::child();
    }
    mac::check_process_exit()
}

#[cfg(target_os = "macos")]
mod mac {
    use anyhow::{Context, Result};
    use nova::app_status::{AppStatus, ServiceState};
    use std::io::Read;
    use std::os::unix::fs::DirBuilderExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Fixture {
        process: Option<Child>,
        directory: std::path::PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Some(process) = &mut self.process {
                let _ = process.kill();
                let _ = process.wait();
            }
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    pub fn check_process_exit() -> Result<()> {
        let directory = std::env::temp_dir().join(format!("nova-quit-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let mut fixture = Fixture {
            process: None,
            directory,
        };
        let mut process = Command::new(std::env::current_exe()?)
            .arg("--test-owned-process")
            .env("NOVA_APP_SOCKET", fixture.directory.join("service.sock"))
            .env("NOVA_CHROME_SOCKET", fixture.directory.join("chrome.sock"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        // This pipe stays open so the fixture's simulated native worker
        // cannot finish by seeing EOF during the deadline check.
        let input = process.stdin.take().unwrap();
        fixture.process = Some(process);
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = fixture.process.as_mut().unwrap().try_wait()? {
                break status;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "Quit waited for an uncancellable worker"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        anyhow::ensure!(status.success(), "test app process failed: {status}");
        assert!(
            !fixture.directory.join("service.sock").exists(),
            "listener guard must release its socket before exit"
        );
        drop(input);
        println!("app process Quit: owned listener released and blocked native worker did not delay exit");
        Ok(())
    }

    pub fn child() -> Result<()> {
        let runtime = tokio::runtime::Runtime::new()?;
        nova::platform::mac::event_loop::run_app_process(runtime, |runtime| {
            let status = AppStatus::default();
            let listener = runtime.spawn(nova::app_service::run_with_status(status.clone()));
            runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(3), async {
                        while status.snapshot().service != ServiceState::Ready {
                            anyhow::ensure!(!listener.is_finished(), "fixture listener failed");
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                    .await??;
                    let (started, waiting) = tokio::sync::oneshot::channel();
                    let worker = tokio::task::spawn_blocking(move || {
                        let _ = started.send(());
                        let _ = std::io::stdin().read_exact(&mut [0u8]);
                    });
                    waiting.await?;
                    assert!(
                        !worker.is_finished(),
                        "fixture worker must still be blocked"
                    );
                    // Same orderly listener cancellation as the menu's Quit path.
                    listener.abort();
                    anyhow::ensure!(
                        listener.await.unwrap_err().is_cancelled(),
                        "listener did not stop"
                    );
                    assert!(!nova::app_service::socket_path()?.exists());
                    Ok(())
                })
                .context("exercise app runtime termination")
        })
    }
}
