mod cache;
mod control;
mod mcp;
mod tools;
mod watcher;

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
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Mcp => {
            // stdout is reserved for MCP JSON-RPC messages - logs MUST go to
            // stderr, or they'd corrupt the protocol stream from the client's
            // point of view.
            init_tracing_stderr();
            tracing::info!("nexusd starting as MCP stdio server");
            mcp::serve_stdio()
        }
        Command::Serve => {
            let paths = nexus_core::Paths::resolve();
            std::fs::create_dir_all(&paths.data_dir)?;
            // A long-lived daemon's logs are worth tailing from a file - the
            // GUI's Logs view reads this directly rather than needing a
            // streaming protocol.
            init_tracing_file(&paths.log_file())?;
            tracing::info!("nexusd starting as background daemon (control API + file watcher)");
            watcher::spawn();
            control::serve(paths.control_socket())
        }
    }
}

fn env_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    std::env::var("NEXUS_LOG_LEVEL")
        .map(EnvFilter::new)
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// `NEXUS_LOG_FORMAT=json` gives structured, machine-parseable logs for
/// support bundles/log aggregation; plain text (the default) is easier to
/// read live.
fn wants_json_logs() -> bool {
    std::env::var("NEXUS_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
}

fn init_tracing_stderr() {
    if wants_json_logs() {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_writer(std::io::stderr)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_writer(std::io::stderr)
            .init();
    }
}

fn init_tracing_file(log_path: &std::path::Path) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    if wants_json_logs() {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_ansi(false)
            .with_writer(move || file.try_clone().expect("failed to clone log file handle"))
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_ansi(false)
            .with_writer(move || file.try_clone().expect("failed to clone log file handle"))
            .init();
    }
    Ok(())
}
