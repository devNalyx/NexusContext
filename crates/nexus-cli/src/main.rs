mod install;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use nexus_core::{Config, Paths};
use nexus_index::{
    self as index, delete_project, export_obsidian, export_project, graph_db_path, import_project,
    index_project, Direction, NodeRecord,
};
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
    /// Summarize a project: node/edge counts and busiest files by definition count.
    Architecture {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Map uncommitted git changes to affected graph symbols.
    DetectChanges {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Functions with no inbound CALLS edge (name-based resolution caveats apply - see README).
    DeadCode {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Pick the cheapest retrieval strategy for a query (file read, graph search, or keyword fallback).
    QueryPlanner {
        query: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        start_line: Option<usize>,
        #[arg(long)]
        end_line: Option<usize>,
    },
    /// Export the local index for teammates (zstd) or for browsing in Obsidian (markdown vault).
    Export {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = ExportFormat::Zstd)]
        format: ExportFormat,
    },
    /// Load a teammate's exported index instead of reindexing from scratch.
    Import {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Remove a project's indexed data (does not touch the source directory).
    Delete {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Auto-detect MCP-capable agents on this machine and configure `nexusd mcp` for each.
    Install,
    /// Grep-like full-text search over indexed file content (not symbol names).
    SearchCode {
        query: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DirectionArg {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExportFormat {
    Zstd,
    Obsidian,
}

impl From<DirectionArg> for Direction {
    fn from(d: DirectionArg) -> Self {
        match d {
            DirectionArg::Inbound => Direction::Inbound,
            DirectionArg::Outbound => Direction::Outbound,
        }
    }
}

fn print_records(records: &[NodeRecord]) {
    if records.is_empty() {
        println!("(no results)");
    }
    for node in records {
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
            let stats = index_project(&path)?;
            println!("indexed {} files", stats.files_indexed);
            println!("nodes: {}, edges: {}", stats.nodes, stats.edges);
            println!("graph stored at {}", graph_db_path(&path).display());
        }
        Command::SearchGraph {
            pattern,
            project,
            limit,
        } => {
            let store = index::open_store(&project)?;
            let results = store.search_by_name(&pattern, limit)?;
            print_records(&results);
        }
        Command::Trace {
            name,
            project,
            direction,
            depth,
        } => {
            let store = index::open_store(&project)?;
            let results = store.trace_calls(&name, direction.into(), depth)?;
            if results.is_empty() {
                println!(
                    "no {direction:?} calls found for '{name}' within depth {depth} \
                     (resolution is name-based - an ambiguous same-named function across \
                     files won't resolve cross-file; see README)"
                );
            }
            print_records(&results);
        }
        Command::Architecture { project } => {
            let summary = index::get_architecture(&project)?;
            println!("total nodes: {}", summary.total_nodes);
            println!("total edges: {}", summary.total_edges);
            println!("busiest files:");
            for (file, count) in summary.busiest_files {
                println!("  {count:>4}  {file}");
            }
            println!("language breakdown:");
            for (ext, count) in summary.language_breakdown {
                println!("  {count:>4}  .{ext}");
            }
        }
        Command::DetectChanges { project } => {
            let affected = index::detect_changes(&project)?;
            print_records(&affected);
        }
        Command::DeadCode { project } => {
            let dead = index::detect_dead_code(&project)?;
            print_records(&dead);
        }
        Command::QueryPlanner {
            query,
            project,
            file,
            start_line,
            end_line,
        } => {
            let plan = index::plan_query(&project, &query, file.as_deref(), start_line, end_line)?;
            println!("strategy: {}", plan.strategy);
            if let Some(note) = plan.note {
                println!("note: {note}");
            }
            if let Some(content) = plan.file_content {
                println!("{content}");
            } else {
                print_records(&plan.records);
            }
        }
        Command::Export { path, format } => match format {
            ExportFormat::Zstd => {
                let artifact = export_project(&path)?;
                println!("exported index to {}", artifact.display());
            }
            ExportFormat::Obsidian => {
                let vault = export_obsidian(&path)?;
                println!("exported Obsidian vault to {}", vault.display());
            }
        },
        Command::Import { path } => {
            let stats = import_project(&path)?;
            println!(
                "imported index: nodes: {}, edges: {}",
                stats.nodes, stats.edges
            );
        }
        Command::Delete { path } => {
            delete_project(&path)?;
            println!("deleted index for {}", path.display());
        }
        Command::Install => install::run()?,
        Command::SearchCode {
            query,
            project,
            limit,
        } => {
            let hits = index::search_code(&project, &query, limit)?;
            if hits.is_empty() {
                println!("no matches for '{query}'");
            }
            for hit in hits {
                println!("{}\n  {}\n", hit.file_path, hit.snippet);
            }
        }
    }

    Ok(())
}
