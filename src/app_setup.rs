//! Passive status and explicit, initial user-level Nova.app installation.
//! This does not replace an app, launch desktop code, or alter permissions.

use clap::Subcommand;
use serde::Serialize;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug, Subcommand)]
pub enum AppCommand {
    /// Inspect canonical user/system app paths without launching the app.
    Status,
    /// Install a checksum-verified local app ZIP into ~/Applications, only
    /// when no canonical Nova.app installation already exists.
    Install {
        #[arg(long)]
        archive: PathBuf,
        /// SHA256 obtained from a trusted release/catalogue (64 hex digits).
        #[arg(long)]
        sha256: String,
        /// Explicit local_ad_hoc mode: ad-hoc sign only the verified staged
        /// app and remove its quarantine, without accessing any keychain.
        #[arg(long)]
        local_sign: bool,
    },
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    Missing,
    Installed,
    AlreadyInstalled,
    Unsupported,
    Error,
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub success: bool,
    pub state: SetupState,
    pub message: String,
    pub next_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub development: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_compatible: Option<bool>,
}

impl SetupReport {
    fn new(state: SetupState, success: bool, message: &str, next_action: &str) -> Self {
        Self {
            success,
            state,
            message: message.into(),
            next_action: next_action.into(),
            path: None,
            version: None,
            signing: None,
            development: None,
            connector_compatible: None,
        }
    }
}

pub fn run(command: &AppCommand) -> SetupReport {
    #[cfg(target_os = "macos")]
    {
        macos::run(command)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        SetupReport::new(
            SetupState::Unsupported,
            false,
            "Nova.app setup is supported on macOS only.",
            "Use nova mcp directly on Windows; no separate Nova.app is required.",
        )
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_setup_never_opens_the_archive() {
        for command in [
            AppCommand::Status,
            AppCommand::Install {
                archive: PathBuf::from("/does-not-exist.zip"),
                sha256: "invalid".into(),
                local_sign: false,
            },
        ] {
            assert_eq!(run(&command).state, SetupState::Unsupported);
        }
    }
}
