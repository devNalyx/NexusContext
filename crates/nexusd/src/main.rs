mod mcp;
mod tools;

use anyhow::Result;

fn main() -> Result<()> {
    init_tracing();
    tracing::info!("nexusd starting as MCP stdio server");
    mcp::serve_stdio()
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = std::env::var("NEXUS_LOG_LEVEL")
        .map(EnvFilter::new)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // stdout is reserved for MCP JSON-RPC messages - logs MUST go to stderr,
    // or they'd corrupt the protocol stream from the client's point of view.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
