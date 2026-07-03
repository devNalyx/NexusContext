mod control;
mod mcp;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "nexusd", about = "NexusContext daemon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run as an MCP stdio server - what an IDE/agent should launch as a subprocess.
    Mcp,
    /// Run as a long-lived background daemon exposing the control socket -
    /// what systemd (or the GUI, on demand) should launch.
    Serve,
}

fn main() -> Result<()> {
    init_tracing();

    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Mcp => {
            tracing::info!("nexusd starting as MCP stdio server");
            mcp::serve_stdio()
        }
        Command::Serve => {
            let paths = nexus_core::Paths::resolve();
            std::fs::create_dir_all(&paths.data_dir)?;
            tracing::info!("nexusd starting as background daemon (control API only)");
            control::serve(paths.control_socket())
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = std::env::var("NEXUS_LOG_LEVEL")
        .map(EnvFilter::new)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // stdout is reserved for MCP JSON-RPC messages in `mcp` mode - logs MUST
    // go to stderr, or they'd corrupt the protocol stream from the client's
    // point of view. Keeping this the same in `serve` mode too for consistency.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
