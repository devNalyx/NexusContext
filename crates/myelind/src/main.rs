use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "myelind", about = "Myelin daemon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run as an MCP stdio server — what an agent/IDE should launch as a subprocess.
    Mcp,
    /// Run as a long-lived background daemon exposing the control socket —
    /// what systemd should launch.
    Serve,
}

fn main() -> Result<()> {
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Mcp => {
            // stdout is reserved for MCP JSON-RPC messages — logs go to stderr.
            init_tracing_stderr();
            tracing::info!("myelind starting as MCP stdio server");
            myelind::mcp::serve_stdio()
        }
        Command::Serve => {
            let paths = myelin_core::Paths::resolve();
            std::fs::create_dir_all(&paths.data_dir)?;
            init_tracing_file(&paths.log_file())?;
            tracing::info!("myelind starting as background daemon (control API)");
            myelind::control::serve(paths.control_socket())
        }
    }
}

fn env_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    std::env::var("MYELIN_LOG_LEVEL")
        .map(EnvFilter::new)
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// `MYELIN_LOG_FORMAT=json` gives structured logs; plain text is the default.
fn wants_json_logs() -> bool {
    std::env::var("MYELIN_LOG_FORMAT")
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
