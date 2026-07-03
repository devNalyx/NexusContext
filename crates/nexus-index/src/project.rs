use crate::graph::GraphStore;
use crate::ingest::{index_directory, IndexStats};
use anyhow::{bail, Result};
use nexus_core::{project_hash, Config, Paths, ProjectEntry, Registry};
use std::path::Path;

/// Single entry point for "index this directory" used by the CLI, the MCP
/// `index_repository` tool, and the control API's `projects.reindex` -
/// keeps the project registry (used to list known projects by path) in
/// sync no matter which caller triggered the reindex, and enforces
/// `allowed_roots` (if the user opted into that) regardless of which
/// caller triggered it.
pub fn index_project(repo_path: &Path) -> Result<IndexStats> {
    let paths = Paths::resolve();

    let config = Config::load(&paths.config_file())?;
    if !config.is_path_allowed(repo_path) {
        bail!(
            "{} is outside the configured allowed_roots - refusing to index it",
            repo_path.display()
        );
    }

    let hash = project_hash(repo_path);
    let db_path = paths.project_data_dir(&hash).join("graph.db");

    let store = GraphStore::open(&db_path)?;
    let stats = index_directory(repo_path, &store)?;

    let mut registry = Registry::load(&paths.registry_file());
    registry.upsert(ProjectEntry {
        root_path: repo_path.display().to_string(),
        hash,
        last_indexed_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        nodes: stats.nodes,
        edges: stats.edges,
    });
    registry.save(&paths.registry_file())?;

    Ok(stats)
}

pub fn graph_db_path(repo_path: &Path) -> std::path::PathBuf {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    paths.project_data_dir(&hash).join("graph.db")
}
