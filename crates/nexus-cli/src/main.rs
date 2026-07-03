use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use nexus_core::{project_hash, Config, Paths};
use nexus_index::{index_directory, Direction, GraphStore};
use std::path::PathBuf;

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
    /// Build (or rebuild) the knowledge graph for a directory.
    Reindex {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Structural name search over the knowledge graph (no embeddings needed).
    SearchGraph {
        pattern: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Trace callers/callees of a function via the CALLS graph.
    Trace {
        name: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, value_enum, default_value_t = DirectionArg::Outbound)]
        direction: DirectionArg,
        #[arg(long, default_value_t = 3)]
        depth: u32,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DirectionArg {
    Inbound,
    Outbound,
}

impl From<DirectionArg> for Direction {
    fn from(d: DirectionArg) -> Self {
        match d {
            DirectionArg::Inbound => Direction::Inbound,
            DirectionArg::Outbound => Direction::Outbound,
        }
    }
}

fn graph_db_path(paths: &Paths, project: &std::path::Path) -> PathBuf {
    let hash = project_hash(project);
    paths.project_data_dir(&hash).join("graph.db")
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
            let db_path = graph_db_path(&paths, &path);
            let store = GraphStore::open(&db_path)?;
            let stats = index_directory(&path, &store)?;
            println!("indexed {} files", stats.files_indexed);
            println!("nodes: {}, edges: {}", stats.nodes, stats.edges);
            println!("graph stored at {}", db_path.display());
        }
        Command::SearchGraph {
            pattern,
            project,
            limit,
        } => {
            let db_path = graph_db_path(&paths, &project);
            if !db_path.exists() {
                anyhow::bail!(
                    "no index found for {} - run `nexus reindex {}` first",
                    project.display(),
                    project.display()
                );
            }
            let store = GraphStore::open(&db_path)?;
            let results = store.search_by_name(&pattern, limit)?;
            if results.is_empty() {
                println!("no matches for '{pattern}'");
            }
            for node in results {
                println!(
                    "{:<9} {:<30} {}:{}-{}",
                    format!("{:?}", node.kind),
                    node.name,
                    node.file_path,
                    node.start_line,
                    node.end_line
                );
            }
        }
        Command::Trace {
            name,
            project,
            direction,
            depth,
        } => {
            let db_path = graph_db_path(&paths, &project);
            if !db_path.exists() {
                anyhow::bail!(
                    "no index found for {} - run `nexus reindex {}` first",
                    project.display(),
                    project.display()
                );
            }
            let store = GraphStore::open(&db_path)?;
            let results = store.trace_calls(&name, direction.into(), depth)?;
            if results.is_empty() {
                println!(
                    "no {direction:?} calls found for '{name}' within depth {depth} \
                     (same-file resolution only, see proposal)"
                );
            }
            for node in results {
                println!(
                    "{:<9} {:<30} {}:{}-{}",
                    format!("{:?}", node.kind),
                    node.name,
                    node.file_path,
                    node.start_line,
                    node.end_line
                );
            }
        }
    }

    Ok(())
}
