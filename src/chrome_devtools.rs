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
use std::net::SocketAddr;
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
    /// access every open window in the selected running Chrome profile. Cannot
    /// be combined with an endpoint, even when explicitly set to `isolated`.
    #[arg(long, value_enum)]
    pub profile: Option<ChromeProfile>,

    /// Advanced/internal: attach to a trusted, already-running browser's
    /// HTTP(S) root. Requires a literal loopback IP and explicit nonzero port.
    /// Cannot be combined with --ws-endpoint, --profile or --headless.
    #[arg(long, value_name = "URL")]
    pub browser_url: Option<String>,

    /// Advanced/internal: attach to a trusted browser-level WS(S) endpoint at
    /// /devtools/browser/<id>, with a literal loopback IP and nonzero port.
    /// Cannot be combined with --browser-url, --profile or --headless.
    #[arg(long, value_name = "URL")]
    pub ws_endpoint: Option<String>,

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

/// Validate the raw authority without URL normalization (which can turn an
/// integer, shorthand or hexadecimal host into a loopback address). The pinned
/// upstream still follows discovery results and redirects: this is input
/// validation for trusted local endpoints, not transport confinement.
fn validate_endpoint(endpoint: &str, websocket: bool) -> Result<()> {
    let message = if websocket {
        "invalid --ws-endpoint: use WS(S) with a literal loopback IP, explicit nonzero port \
         and /devtools/browser/<id> (letters, digits, hyphen or underscore); \
         remove credentials, query and fragment"
    } else {
        "invalid --browser-url: use an HTTP(S) browser root with a literal loopback IP \
         and explicit nonzero port; remove credentials, query and fragment"
    };
    let invalid = || anyhow::anyhow!(message);
    let (scheme, remainder) = endpoint.split_once("://").ok_or_else(invalid)?;
    let valid_scheme = if websocket {
        scheme.eq_ignore_ascii_case("ws") || scheme.eq_ignore_ascii_case("wss")
    } else {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    };
    if !valid_scheme
        || endpoint.contains(['@', '?', '#', '\\'])
        || endpoint
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        bail!(message);
    }
    let path_start = remainder.find('/').unwrap_or(remainder.len());
    let (authority, path) = remainder.split_at(path_start);
    let socket: SocketAddr = authority.parse().map_err(|_| invalid())?;
    if !socket.ip().is_loopback() || socket.port() == 0 {
        bail!(message);
    }
    if websocket {
        let id = path
            .strip_prefix("/devtools/browser/")
            .ok_or_else(invalid)?;
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            bail!(message);
        }
    } else if !path.is_empty() && path != "/" {
        bail!(message);
    }
    Ok(())
}

/// Validate Nova's policy flags and build the literal argument vector passed
/// to npx. Keeping this separate makes the security defaults regression
/// testable without spawning Node or Chrome.
fn upstream_args(options: &ChromeDevtoolsArgs) -> Result<Vec<OsString>> {
    if options.browser_url.is_some() && options.ws_endpoint.is_some() {
        bail!("choose only one endpoint: --browser-url or --ws-endpoint");
    }
    let attaching = options.browser_url.is_some() || options.ws_endpoint.is_some();
    if attaching && (options.profile.is_some() || options.headless) {
        bail!(
            "endpoint attachment cannot be combined with --profile or --headless; \
               remove those options to attach to the running browser"
        );
    }
    if options.profile == Some(ChromeProfile::Existing) && options.headless {
        bail!("--headless cannot be used with --profile existing");
    }

    let mut args = vec![
        OsString::from("--yes"),
        OsString::from(CHROME_DEVTOOLS_MCP_PACKAGE),
    ];

    if let Some(endpoint) = &options.browser_url {
        validate_endpoint(endpoint, false)?;
        args.push(OsString::from("--browser-url"));
        args.push(OsString::from(endpoint));
    } else if let Some(endpoint) = &options.ws_endpoint {
        validate_endpoint(endpoint, true)?;
        args.push(OsString::from("--ws-endpoint"));
        args.push(OsString::from(endpoint));
    } else {
        match options.profile.unwrap_or_default() {
            ChromeProfile::Isolated => args.push(OsString::from("--isolated")),
            ChromeProfile::Existing => args.push(OsString::from("--auto-connect")),
        }
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
        if !attaching && options.profile.unwrap_or_default() == ChromeProfile::Isolated {
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
    let args = upstream_args(options)?;
    let mut command = Command::new(&options.npx);
    command
        .args(args)
        .env("CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS", "1")
        .env("CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS", "1");
    Ok(command)
}

/// Replace this process with (Unix), or run and wait for (Windows), the pinned
/// official Chrome DevTools MCP server. The child inherits stdin/stdout/stderr.
pub fn run(options: &ChromeDevtoolsArgs) -> Result<()> {
    let mut command = command(options)?;

    if options.enable_webmcp
        && (options.profile == Some(ChromeProfile::Existing)
            || options.browser_url.is_some()
            || options.ws_endpoint.is_some())
    {
        eprintln!(
            "Nova: --enable-webmcp cannot add launch flags to a running browser; \
             start the selected browser with --enable-features=WebMCP before connecting."
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
            profile: None,
            browser_url: None,
            ws_endpoint: None,
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
        let mut explicit = options();
        explicit.profile = Some(ChromeProfile::Isolated);
        assert_eq!(strings(&explicit), strings(&options()));
    }

    #[test]
    fn endpoint_modes_forward_literal_urls_without_browser_launch_options() {
        for (websocket, endpoint) in [
            (false, "http://127.0.0.1:9222"),
            (false, "https://[0:0:0:0:0:0:0:1]:443/"),
            (true, "ws://127.0.0.2:9222/devtools/browser/a-b_1"),
            (true, "wss://[::1]:443/devtools/browser/a-b_1"),
        ] {
            let mut options = options();
            if websocket {
                options.ws_endpoint = Some(endpoint.into());
            } else {
                options.browser_url = Some(endpoint.into());
            }
            options.enable_webmcp = true;
            assert_eq!(
                strings(&options),
                [
                    "--yes",
                    CHROME_DEVTOOLS_MCP_PACKAGE,
                    if websocket {
                        "--ws-endpoint"
                    } else {
                        "--browser-url"
                    },
                    endpoint,
                    "--no-usage-statistics",
                    "--no-performance-crux",
                    "--redact-network-headers",
                    "--category-experimental-webmcp=true",
                ]
            );
        }
    }

    #[test]
    fn endpoint_syntax_rejects_normalization_credentials_and_non_browser_paths() {
        for (websocket, endpoint) in [
            (false, "http://localhost:9222"),
            (false, "http://127.1:9222"),
            (false, "http://2130706433:9222"),
            (false, "http://0x7f000001:9222"),
            (false, "http://0177.0.0.1:9222"),
            (false, "http://127.0.0.01:9222"),
            (false, "http://128.0.0.1:9222"),
            (false, "http://[::ffff:127.0.0.1]:9222"),
            (false, "http://[::1%25lo0]:9222"),
            (false, "http://127.0.0.1"),
            (false, "https://[::1]"),
            (false, "http://127.0.0.1:0"),
            (false, "http://127.0.0.1:65536"),
            (false, "http://private:secret@127.0.0.1:9222"),
            (false, "http://127.0.0.1:9222/?private=secret"),
            (false, "http://127.0.0.1:9222/#private-secret"),
            (false, "http://127.0.0.1:9222/json/version"),
            (false, "http://127.0.0.1:9222/../"),
            (false, "http://127.0.0.1:9222\\private-secret"),
            (false, " http://127.0.0.1:9222"),
            (false, "http://127.0.0.1:9222\n"),
            (false, "ws://127.0.0.1:9222/"),
            (false, ""),
            (true, "http://127.0.0.1:9222/devtools/browser/id"),
            (true, "ws://localhost:9222/devtools/browser/id"),
            (true, "ws://127.0.0.1:9222/"),
            (true, "ws://127.0.0.1:9222/devtools/page/id"),
            (true, "ws://127.0.0.1:9222/node-inspector-id"),
            (true, "ws://127.0.0.1:9222/devtools/browser/"),
            (true, "ws://127.0.0.1:9222/devtools/browser/../page/id"),
            (true, "ws://127.0.0.1:9222/devtools/browser/%2e%2e"),
            (true, "ws://private:secret@[::1]:9222/devtools/browser/id"),
            (true, "ws://[::1]:9222/devtools/browser/id?private=secret"),
            (true, "ws://[::1]:9222/devtools/browser/id#private-secret"),
        ] {
            let error = validate_endpoint(endpoint, websocket)
                .unwrap_err()
                .to_string();
            assert!(error.contains("literal loopback IP"));
            assert!(!error.contains("private"));
            assert!(!error.contains("secret"));
            assert!(!error.contains(endpoint) || endpoint.is_empty());
        }
    }

    #[test]
    fn run_policy_rejects_endpoint_conflicts_without_spawning_or_echoing_inputs() {
        let mut options = options();
        options.npx = PathBuf::new();
        options.browser_url = Some("http://private:secret@127.0.0.1:9222".into());
        options.ws_endpoint = Some("ws://[::1]:9222/devtools/browser/id".into());
        assert!(run(&options)
            .unwrap_err()
            .to_string()
            .contains("choose only one"));
        options.ws_endpoint = None;
        for profile in [ChromeProfile::Isolated, ChromeProfile::Existing] {
            options.profile = Some(profile);
            let error = run(&options).unwrap_err().to_string();
            assert!(error.contains("remove those options"));
            assert!(!error.contains("secret"));
        }
        options.profile = None;
        options.headless = true;
        assert!(run(&options)
            .unwrap_err()
            .to_string()
            .contains("--headless"));
        options.headless = false;
        let error = run(&options).unwrap_err().to_string();
        assert!(error.contains("invalid --browser-url"));
        assert!(!error.contains("private"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn existing_profile_uses_auto_connect_instead_of_isolation() {
        let mut options = options();
        options.profile = Some(ChromeProfile::Existing);
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

        options.profile = Some(ChromeProfile::Existing);
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

        options.profile = Some(ChromeProfile::Existing);
        options.headless = true;
        assert!(upstream_args(&options)
            .unwrap_err()
            .to_string()
            .contains("--headless"));
    }
}
