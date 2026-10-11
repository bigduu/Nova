//! Explicit ad-hoc signing of an already verified, private installation stage.
//! No certificate, keychain, trust setting, application launch, or TCC access.

use super::checked_output;
use anyhow::{ensure, Context, Result};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

pub(super) trait Signer {
    fn sign(&self, app: &Path) -> Result<()>;
}

pub(super) struct NativeSigner;

impl Signer for NativeSigner {
    fn sign(&self, app: &Path) -> Result<()> {
        sign_with(&NativeRunner, app)
    }
}

// This release's only executable is Contents/MacOS/nova. Fail closed on a new
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
            .output()
            .with_context(|| format!("Run {program} for local ad-hoc signing"))
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

fn sign_with(runner: &dyn Runner, app: &Path) -> Result<()> {
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
                "--sign".as_ref(),
                "-".as_ref(),
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
                path.as_os_str(),
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
