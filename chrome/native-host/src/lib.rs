//! Chrome Native Messaging ↔ Nova's explicitly owned local broker.
//!
//! This crate intentionally owns no desktop-control capability. The installed
//! host validates and forwards protocol messages to Nova's private local
//! endpoint. [`AppBridgeListener`] is the server-side primitive the owning
//! process embeds without mixing Chrome traffic into its MCP transport.

pub mod app;
pub mod framing;
pub mod protocol;

#[cfg(unix)]
mod socket;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use socket::configured_socket_path;
#[cfg(unix)]
pub use socket::{default_socket_path, AppBridgeConnection, AppBridgeListener};
#[cfg(windows)]
pub(crate) use windows::configured_pipe_path as configured_socket_path;
#[cfg(windows)]
pub use windows::{
    default_pipe_path, managed_chrome_configured, AppBridgeConnection, AppBridgeListener,
};

pub use app::ChromeBridge;

/// Run the stdio native messaging host.
pub fn run_native_host() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        socket::run_host()
    }
    #[cfg(windows)]
    {
        windows::run_host()
    }
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!(
            "Nova's Chrome native bridge is not yet available on this platform; no desktop fallback was attempted"
        )
    }
}
