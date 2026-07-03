use anyhow::Result;
use clap::{Parser, Subcommand};
use nexus_core::{Config, Paths};

#[derive(Parser)]
#[command(name = "nexus", about = "NexusContext CLI - manual indexing and queries")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show resolved config/data paths and current config.
    Status,
    /// Trigger a manual reindex of a directory. (Phase 1 - not yet implemented.)
    Reindex {
        #[arg(default_value = ".")]
        path: String,
    },
    /// Run a search query against the index. (Phase 1 - not yet implemented.)
    Search { query: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve();

    match cli.command {
        Command::Status => {
            let config = Config::load(&paths.config_file())?;
            println!("config file : {}", paths.config_file().display());
            println!("data dir    : {}", paths.data_dir.display());
            println!(
                "embeddings  : {}",
                config
                    .embeddings
                    .endpoint
                    .as_deref()
                    .unwrap_or("(not configured - structural tools still work)")
            );
        }
        Command::Reindex { path } => {
            println!("reindex not yet implemented (Phase 1) - requested path: {path}");
        }
        Command::Search { query } => {
            println!("search not yet implemented (Phase 1) - query: {query}");
        }
    }

    Ok(())
}
