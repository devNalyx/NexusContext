//! `nexus source …` — the human/CLI side of registering external knowledge
//! sources (an Obsidian vault, a notes directory). Registration is never an
//! MCP write path: agents can discover sources, not create or relabel them.

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use nexus_core::{Config, Paths, SourceConfig};
use nexus_index::index_project;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum Command {
    /// Register a directory as a named source and index it immediately.
    Add {
        /// Stable source id, used in curated-fact provenance.
        id: String,
        /// Directory to register (canonicalized before persistence).
        path: PathBuf,
        /// Source flavour, e.g. `obsidian-vault` or `markdown`.
        #[arg(long, default_value = "obsidian-vault")]
        kind: String,
        /// OKF-style provenance URI. Defaults to `<kind>://<id>`.
        #[arg(long)]
        resource: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// List registered sources (id, kind, canonical root, provenance URI).
    List,
    /// Remove a source registration. Does not delete the indexed graph.
    Remove { id: String },
}

fn canonical_root(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        bail!("source path does not exist: {}", path.display());
    }
    let root = path.canonicalize()?;
    if !root.is_dir() {
        bail!("source path is not a directory: {}", path.display());
    }
    Ok(root)
}

fn check_unique(config: &Config, id: &str, root: &Path) -> Result<()> {
    if id.trim().is_empty() {
        bail!("source id must not be empty");
    }
    if config.sources.iter().any(|s| s.id == id) {
        bail!("source id is already registered: {id}");
    }
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if config.sources.iter().any(|s| {
        Path::new(&s.root_path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&s.root_path))
            == canonical
    }) {
        bail!("a source is already registered at {}", root.display());
    }
    Ok(())
}

pub fn run(command: Command) -> Result<()> {
    let config_file = Paths::resolve().config_file();
    let mut config = Config::load(&config_file)?;

    match command {
        Command::Add {
            id,
            path,
            kind,
            resource,
            title,
            description,
        } => {
            let root = canonical_root(&path).map_err(|e| anyhow!("invalid source path: {e}"))?;
            // Registration must not be a back door around `allowed_roots`:
            // the same check runs again inside index_project, but failing
            // here avoids writing a config entry that can never be indexed.
            if !config.allowed_roots.is_empty() && !config.is_path_allowed(&root) {
                bail!("source root is outside the configured allowed_roots");
            }
            check_unique(&config, &id, &root)?;
            let resource = resource.unwrap_or_else(|| format!("{kind}://{id}"));
            config.sources.push(SourceConfig {
                id: id.clone(),
                root_path: root.display().to_string(),
                kind: kind.clone(),
                resource: Some(resource.clone()),
                title,
                description,
                enabled: true,
            });
            config.save(&config_file)?;

            // A registration that leaves the source unindexed would just
            // reproduce the "no index found" failure the feature exists to
            // remove, so index here rather than waiting for a first query.
            let stats = index_project(&root)?;
            println!("registered source {id} ({kind}) at {}", root.display());
            println!(
                "indexed {} files, {} nodes / {} edges",
                stats.files_indexed, stats.nodes, stats.edges
            );
        }
        Command::List => {
            let sources: Vec<_> = config
                .sources
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "kind": s.kind,
                        "root_path": s.root_path,
                        "resource": s.resource,
                        "title": s.title,
                        "description": s.description,
                        "enabled": s.enabled,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "sources": sources,
                    "config_file": config_file,
                }))?
            );
        }
        Command::Remove { id } => {
            let before = config.sources.len();
            config.sources.retain(|s| s.id != id);
            if config.sources.len() == before {
                bail!("no such source: {id}");
            }
            config.save(&config_file)?;
            println!("removed source {id} (indexed data left in place)");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_id_and_root_are_rejected() {
        let dir =
            std::env::temp_dir().join(format!("nexuscontext-source-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let mut config = Config::default();
        config.sources.push(SourceConfig {
            id: "vault".to_string(),
            root_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        });
        assert!(check_unique(&config, "vault", Path::new("/tmp/other")).is_err());
        assert!(check_unique(&config, "other", &dir).is_err());
        assert!(check_unique(&config, "", Path::new("/tmp/other")).is_err());
        assert!(check_unique(&Config::default(), "other", &dir).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
