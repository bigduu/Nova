//! Chrome Native Messaging ↔ Nova.app bridge.
//!
//! This crate intentionally owns no desktop-control capability. The installed
//! host validates and forwards protocol messages to the private per-user Nova
//! app socket. [`AppBridgeListener`] is the server-side primitive Nova.app can
//! embed without mixing Chrome traffic into its MCP socket.

pub mod app;
pub mod framing;
pub mod protocol;

#[cfg(unix)]
mod socket;

#[cfg(unix)]
pub(crate) use socket::configured_socket_path;
#[cfg(unix)]
pub use socket::{default_socket_path, AppBridgeConnection, AppBridgeListener};

pub use app::ChromeBridge;

/// Run the stdio native messaging host.
pub fn run_native_host() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        socket::run_host()
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!(
            "Nova's Chrome native bridge is not yet available on this platform; no desktop fallback was attempted"
        )
    }
}
