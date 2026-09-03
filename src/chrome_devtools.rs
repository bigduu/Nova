//! Safe launcher for the official Chrome DevTools MCP server.
//!
//! Nova does not reimplement Chrome's debugging protocol here.  This module is
//! deliberately a thin stdio sidecar: it turns Nova's small, security-oriented
//! option set into an invocation of an exact upstream npm package, then replaces
//! the Nova process on Unix (or waits for it on Windows).  Stdin/stdout are left
//! inherited so the MCP byte stream stays transparent.

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Keep the selected upstream version fixed until its CLI and tool surface
/// have been reviewed and the tests below are updated for a newer release.
/// Pinning registry artifact integrity is a separate production-hardening
/// step; the launcher intentionally does not claim that the npm bytes are
/// content-addressed here.
pub const CHROME_DEVTOOLS_MCP_PACKAGE: &str = "chrome-devtools-mcp@1.8.0";

#[cfg(windows)]
const DEFAULT_NPX: &str = "npx.cmd";
#[cfg(not(windows))]
const DEFAULT_NPX: &str = "npx";

/// Which Chrome identity the official DevTools MCP server may control.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ChromeProfile {
    /// Launch a fresh temporary Chrome profile and remove it on exit.
    #[default]
    Isolated,
    /// Attach to the locally running stable Chrome profile after the user has
    /// explicitly enabled remote debugging in Chrome. Requires Chrome 144+;
    /// Chrome selects its default profile if several profiles are active.
    Existing,
}

/// Launch the official Chrome DevTools MCP server through Nova.
#[derive(Args, Clone, Debug)]
pub struct ChromeDevtoolsArgs {
    /// Chrome profile policy. `isolated` is the safer default; `existing` can
    /// access every open window in the selected running Chrome profile.
    #[arg(long, value_enum, default_value_t)]
    pub profile: ChromeProfile,

    /// Run the isolated Chrome instance without a visible window.
    #[arg(long)]
    pub headless: bool,

    /// Apply an upstream URLPattern guardrail to attached DevTools targets.
    /// Repeat for multiple patterns. This is not a complete network sandbox;
    /// use OS/VM isolation for that boundary. Requires Chrome 149+.
    #[arg(long, value_name = "URL_PATTERN")]
    pub allowed_url_pattern: Vec<String>,

    /// Enable the upstream experimental WebMCP tool category. In isolated
    /// mode Nova also launches Chrome with the required WebMCP feature flag.
    /// Requires Chrome 150+.
    #[arg(long)]
    pub enable_webmcp: bool,

    /// Return sensitive network request/response headers to the MCP client.
    /// By default Nova asks upstream to redact them.
    #[arg(long)]
    pub expose_network_headers: bool,

    /// Allow performance traces to send inspected URLs to the CrUX API.
    /// Disabled by default to avoid disclosing browsing targets externally.
    #[arg(long)]
    pub enable_performance_crux: bool,

    /// Path to the npm package runner. Useful when a GUI MCP host has a
    /// minimal PATH. Defaults to `npx` (`npx.cmd` on Windows). The pinned
    /// package requires npm and Node.js ^20.19.0, ^22.12.0, or >=23.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_NPX)]
    pub npx: PathBuf,
}

/// Validate Nova's policy flags and build the literal argument vector passed
/// to npx. Keeping this separate makes the security defaults regression
/// testable without spawning Node or Chrome.
fn upstream_args(options: &ChromeDevtoolsArgs) -> Result<Vec<OsString>> {
    if options.profile == ChromeProfile::Existing && options.headless {
        bail!("--headless cannot be used with --profile existing");
    }

    let mut args = vec![
        OsString::from("--yes"),
        OsString::from(CHROME_DEVTOOLS_MCP_PACKAGE),
    ];

    match options.profile {
        ChromeProfile::Isolated => args.push(OsString::from("--isolated")),
        ChromeProfile::Existing => args.push(OsString::from("--auto-connect")),
    }

    // These are intentionally duplicated as both CLI policy and environment
    // policy in `command`: the exact pinned release honors both, and the env
    // vars also prevent work before yargs has parsed the full invocation.
    args.push(OsString::from("--no-usage-statistics"));
    if !options.enable_performance_crux {
        args.push(OsString::from("--no-performance-crux"));
    }
    if !options.expose_network_headers {
        args.push(OsString::from("--redact-network-headers"));
    }

    if options.headless {
        args.push(OsString::from("--headless"));
    }

    if options.enable_webmcp {
        args.push(OsString::from("--category-experimental-webmcp=true"));
        if options.profile == ChromeProfile::Isolated {
            args.push(OsString::from("--chrome-arg=--enable-features=WebMCP"));
        }
    }

    args.extend(
        options
            .allowed_url_pattern
            .iter()
            .map(|pattern| OsString::from(format!("--allowed-url-pattern={pattern}"))),
    );

    Ok(args)
}

fn command(options: &ChromeDevtoolsArgs) -> Result<Command> {
    let mut command = Command::new(&options.npx);
    command
        .args(upstream_args(options)?)
        .env("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1")
        .env("CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS", "1");
    Ok(command)
}

/// Replace this process with (Unix), or run and wait for (Windows), the pinned
/// official Chrome DevTools MCP server. The child inherits stdin/stdout/stderr.
pub fn run(options: &ChromeDevtoolsArgs) -> Result<()> {
    let mut command = command(options)?;

    if options.enable_webmcp && options.profile == ChromeProfile::Existing {
        eprintln!(
            "Nova: --enable-webmcp cannot add Chrome launch flags to an existing profile; \
             start Chrome with --enable-features=WebMCP before connecting."
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(error).with_context(|| format!("launch {}", options.npx.display()))
    }

    #[cfg(windows)]
    {
        let status = command
            .status()
            .with_context(|| format!("launch {}", options.npx.display()))?;
        if !status.success() {
            bail!("Chrome DevTools MCP exited with {status}");
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
        bail!("the Chrome DevTools MCP launcher is unsupported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ChromeDevtoolsArgs {
        ChromeDevtoolsArgs {
            profile: ChromeProfile::Isolated,
            headless: false,
            allowed_url_pattern: Vec::new(),
            enable_webmcp: false,
            expose_network_headers: false,
            enable_performance_crux: false,
            npx: PathBuf::from(DEFAULT_NPX),
        }
    }

    fn strings(options: &ChromeDevtoolsArgs) -> Vec<String> {
        upstream_args(options)
            .unwrap()
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn defaults_pin_upstream_and_apply_private_isolated_policy() {
        assert_eq!(
            strings(&options()),
            [
                "--yes",
                "chrome-devtools-mcp@1.8.0",
                "--isolated",
                "--no-usage-statistics",
                "--no-performance-crux",
                "--redact-network-headers",
            ]
        );
    }

    #[test]
    fn existing_profile_uses_auto_connect_instead_of_isolation() {
        let mut options = options();
        options.profile = ChromeProfile::Existing;
        let args = strings(&options);
        assert!(args.contains(&"--auto-connect".to_string()));
        assert!(!args.contains(&"--isolated".to_string()));
    }

    #[test]
    fn allowed_patterns_remain_separate_literal_arguments() {
        let mut options = options();
        options.allowed_url_pattern = vec![
            "https://example.com/*".to_string(),
            "https://*.example.net/*".to_string(),
        ];
        let args = strings(&options);
        assert!(args.contains(&"--allowed-url-pattern=https://example.com/*".to_string()));
        assert!(args.contains(&"--allowed-url-pattern=https://*.example.net/*".to_string()));
    }

    #[test]
    fn webmcp_only_adds_a_chrome_launch_flag_for_isolated_profiles() {
        let mut options = options();
        options.enable_webmcp = true;
        let isolated = strings(&options);
        assert!(isolated.contains(&"--category-experimental-webmcp=true".to_string()));
        assert!(isolated.contains(&"--chrome-arg=--enable-features=WebMCP".to_string()));

        options.profile = ChromeProfile::Existing;
        let existing = strings(&options);
        assert!(existing.contains(&"--category-experimental-webmcp=true".to_string()));
        assert!(!existing.contains(&"--chrome-arg=--enable-features=WebMCP".to_string()));
    }

    #[test]
    fn explicit_privacy_opt_ins_and_invalid_profile_combinations_are_enforced() {
        let mut options = options();
        options.expose_network_headers = true;
        options.enable_performance_crux = true;
        let args = strings(&options);
        assert!(!args.contains(&"--redact-network-headers".to_string()));
        assert!(!args.contains(&"--no-performance-crux".to_string()));

        options.profile = ChromeProfile::Existing;
        options.headless = true;
        assert!(upstream_args(&options)
            .unwrap_err()
            .to_string()
            .contains("--headless"));
    }
}
