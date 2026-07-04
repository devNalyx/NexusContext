use crate::graph::GraphStore;
use crate::ingest::{index_directory, IndexStats};
use anyhow::{bail, Result};
use nexus_core::{project_hash, Config, Paths, ProjectEntry, Registry};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes full-rebuild reindexes process-wide. The background watcher
/// (on file-change) and a manual reindex (CLI/MCP/control API) can both call
/// `index_project` for the same project around the same time; SQLite's own
/// write lock already keeps that from corrupting `graph.db`, but each side
/// still runs a full clear-and-rebuild independently, which is wasted work
/// at best and, combined with the unsynchronized registry.json
/// read-modify-write below, a real source of lost/corrupted project-list
/// updates. A single process-wide lock is enough here - indexing is already
/// the rare, expensive operation, so serializing it across *all* projects
/// (not just same-project) costs nothing that matters in practice.
static REINDEX_LOCK: Mutex<()> = Mutex::new(());

/// Single entry point for "index this directory" used by the CLI, the MCP
/// `index_repository` tool, and the control API's `projects.reindex` -
/// keeps the project registry (used to list known projects by path) in
/// sync no matter which caller triggered the reindex, and enforces
/// `allowed_roots` (if the user opted into that) regardless of which
/// caller triggered it.
pub fn index_project(repo_path: &Path) -> Result<IndexStats> {
    // A panic mid-reindex on one project shouldn't wedge every future
    // reindex forever - recover the lock rather than propagating the
    // poison.
    let _guard = REINDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let paths = Paths::resolve();
    require_path_allowed(&paths, repo_path)?;

    let db_path = graph_db_path(repo_path);
    let store = GraphStore::open(&db_path)?;
    let stats = index_directory(repo_path, &store)?;

    record_indexed(&paths, repo_path, stats.nodes, stats.edges)?;
    Ok(stats)
}

/// Records that `repo_path` was actually used (searched/queried/traced),
/// distinct from `last_indexed_unix` which only moves on a reindex. A no-op
/// if the project isn't registered yet. Best-effort: a registry write
/// failing here shouldn't fail the tool call that triggered it, so this
/// swallows its own error rather than returning one.
pub fn touch_queried(repo_path: &Path) {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    let mut registry = Registry::load(&paths.registry_file());
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    registry.touch_queried(&hash, now.as_secs());
    let _ = registry.save(&paths.registry_file());
}

/// Total bytes on disk for a project's indexed data (graph.db plus its WAL
/// journal / shared-memory sidecar files) - lets the registry surface real
/// disk usage per project instead of just node/edge counts, so someone who's
/// indexed many repos over time can see which ones are actually worth
/// deleting.
pub fn project_disk_usage(project_hash: &str) -> u64 {
    let dir = Paths::resolve().project_data_dir(project_hash);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

pub fn graph_db_path(repo_path: &Path) -> PathBuf {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    paths.project_data_dir(&hash).join("graph.db")
}

pub fn artifact_path(repo_path: &Path) -> PathBuf {
    repo_path.join(".nexuscontext").join("index.db.zst")
}

/// Compresses the already-built graph into `<repo_path>/.nexuscontext/index.db.zst`
/// so a teammate cloning the repo can `import_project` instead of paying the
/// full reindex cost. This is a point-in-time snapshot, not a live sync -
/// there's no incremental diffing yet (see the proposal's open risks), so an
/// imported snapshot only saves the *first* reindex; anyone who wants fresh
/// data after that still runs a normal full `index_project`.
pub fn export_project(repo_path: &Path) -> Result<PathBuf> {
    let db_path = graph_db_path(repo_path);
    if !db_path.exists() {
        bail!(
            "no index found for {} - run index_project first",
            repo_path.display()
        );
    }

    let artifact = artifact_path(repo_path);
    if let Some(parent) = artifact.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut input = std::fs::File::open(&db_path)?;
    let output = std::fs::File::create(&artifact)?;
    zstd::stream::copy_encode(&mut input, output, 9)?;

    ensure_gitattributes_merge_ours(repo_path)?;
    Ok(artifact)
}

/// Decompresses a shared artifact directly into place instead of walking the
/// tree-sitter pipeline - the whole point is skipping that cost. Registry is
/// updated the same way `index_project` does, using the imported DB's real
/// stats rather than trusting the artifact blindly.
pub fn import_project(repo_path: &Path) -> Result<IndexStats> {
    let paths = Paths::resolve();
    require_path_allowed(&paths, repo_path)?;

    let artifact = artifact_path(repo_path);
    if !artifact.exists() {
        bail!(
            "no shared index artifact found at {} - nothing to import",
            artifact.display()
        );
    }

    let db_path = graph_db_path(repo_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut input = std::fs::File::open(&artifact)?;
    let mut output = std::fs::File::create(&db_path)?;
    zstd::stream::copy_decode(&mut input, &mut output)?;

    let store = GraphStore::open(&db_path)?;
    let (nodes, edges) = store.stats()?;
    record_indexed(&paths, repo_path, nodes, edges)?;

    // files_indexed = 0 signals "imported from artifact", not "walked from source".
    Ok(IndexStats {
        files_indexed: 0,
        nodes,
        edges,
        embeddings_status: "skipped: imported from artifact, not a fresh index".to_string(),
    })
}

/// Removes a project's indexed data (graph.db + WAL/SHM sidecar files) and
/// its registry entry. Does not touch the source directory or the
/// `.nexuscontext/` export artifacts next to it - only what's under the
/// shared data dir.
pub fn delete_project(repo_path: &Path) -> Result<()> {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    let project_dir = paths.project_data_dir(&hash);

    if project_dir.exists() {
        std::fs::remove_dir_all(&project_dir)?;
    }

    let mut registry = Registry::load(&paths.registry_file());
    registry.projects.retain(|p| p.hash != hash);
    registry.save(&paths.registry_file())?;
    Ok(())
}

fn require_path_allowed(paths: &Paths, repo_path: &Path) -> Result<()> {
    let config = Config::load(&paths.config_file())?;
    if !config.is_path_allowed(repo_path) {
        bail!(
            "{} is outside the configured allowed_roots - refusing it",
            repo_path.display()
        );
    }
    Ok(())
}

fn record_indexed(paths: &Paths, repo_path: &Path, nodes: i64, edges: i64) -> Result<()> {
    let mut registry = Registry::load(&paths.registry_file());
    let hash = project_hash(repo_path);
    // upsert() replaces the whole entry - carry the existing
    // last_queried_unix forward rather than resetting "last used" back to
    // never every time a reindex (including an auto-reindex from the
    // watcher) happens to run.
    let last_queried_unix = registry
        .projects
        .iter()
        .find(|p| p.hash == hash)
        .map(|p| p.last_queried_unix)
        .unwrap_or(0);
    registry.upsert(ProjectEntry {
        root_path: repo_path.display().to_string(),
        hash,
        last_indexed_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        nodes,
        edges,
        last_queried_unix,
    });
    registry.save(&paths.registry_file())
}

/// Binary artifacts don't diff meaningfully - `merge=ours` avoids merge
/// conflicts on it entirely (keep whichever side's snapshot, everyone
/// reindexes/reimports as needed rather than trying to reconcile bytes).
fn ensure_gitattributes_merge_ours(repo_path: &Path) -> Result<()> {
    use std::io::Write;

    let gitattributes = repo_path.join(".gitattributes");
    let existing = std::fs::read_to_string(&gitattributes).unwrap_or_default();
    if existing.contains("index.db.zst") {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gitattributes)?;
    writeln!(file, ".nexuscontext/index.db.zst merge=ours")?;
    Ok(())
}
