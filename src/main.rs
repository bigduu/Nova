use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Nova — Computer Use MCP Server
///
/// A macOS desktop control MCP server that gives LLMs the ability to
/// capture screenshots, control mouse/keyboard, manage windows, and introspect apps.
#[derive(Parser, Debug)]
#[command(name = "nova", version, about)]
struct Cli {
    /// Run in Streamable HTTP mode (default: stdio)
    #[arg(long)]
    http: bool,

    /// HTTP listen address (default: 127.0.0.1:3100)
    #[arg(long, default_value = "127.0.0.1:3100")]
    addr: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Init logging — MUST go to stderr. In stdio transport, stdout is the
    // JSON-RPC channel; any log line written there corrupts the protocol stream
    // and makes clients (e.g. when RUST_LOG is set) hang waiting for a response.
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let cli = Cli::parse();

    tracing::info!(
        "Nova Computer Use MCP Server v{}",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!(
        "Transport: {}",
        if cli.http { "Streamable HTTP" } else { "stdio" }
    );

    if cli.http {
        nova::server::run_http(&cli.addr).await
    } else {
        nova::server::run_stdio().await
    }
}
