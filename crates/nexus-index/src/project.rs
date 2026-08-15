use crate::graph::GraphStore;
use crate::ingest::{index_directory, IndexStats};
use anyhow::{bail, Result};
use nexus_core::{project_hash, Config, Paths, ProjectEntry, Registry, WatcherConfig};
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
    index_project_locked(repo_path)
}

/// The actual rebuild, factored out so `touch_and_catchup`'s cold path can
/// hold `REINDEX_LOCK` across its own "is this still cold?" recheck instead
/// of just around the rebuild itself - see that function's doc comment for
/// why the recheck has to happen after the lock is held, not before.
fn index_project_locked(repo_path: &Path) -> Result<IndexStats> {
    let repo_path = canonicalize_for_registry(repo_path)?;
    let repo_path = repo_path.as_path();

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

/// Records one watcher-triggered auto-reindex attempt (success or failure)
/// against the project's registry entry - the background-reindex frequency/
/// cost signal, kept separate from a manual reindex since that's a
/// different question (how expensive is auto-sync running on its own vs.
/// someone asking for a reindex). Best-effort, same reasoning as
/// `touch_queried`.
pub fn record_auto_reindex(repo_path: &Path, duration_ms: u64, success: bool) {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    let mut registry = Registry::load(&paths.registry_file());
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    registry.record_auto_reindex(&hash, duration_ms, now.as_secs(), success);
    let _ = registry.save(&paths.registry_file());
}

/// Whether `repo_path` is registered and has gone cold (not warm per
/// `ProjectEntry::is_warm`) - i.e. the background watcher has already
/// stopped watching it, per the same warm-window config it uses. An
/// unregistered project (never indexed) isn't "cold", it's just untouched -
/// nothing to catch up here, `index_project` is the entry point for that.
fn is_cold(repo_path: &Path) -> bool {
    let paths = Paths::resolve();
    let hash = project_hash(repo_path);
    let registry = Registry::load(&paths.registry_file());
    let Some(entry) = registry.projects.iter().find(|p| p.hash == hash) else {
        return false;
    };
    let warm_window_secs = Config::load(&paths.config_file())
        .map(|c| c.watcher.warm_window_secs)
        .unwrap_or_else(|_| WatcherConfig::default().warm_window_secs);
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    !entry.is_warm(now_unix, warm_window_secs)
}

/// Marks `repo_path` as actually used, catching it up first with a
/// synchronous reindex if the watcher had already stopped watching it (gone
/// cold per `is_cold`/`ProjectEntry::is_warm`). One shared entry point for
/// every caller that answers a query about a project - MCP tool dispatch
/// (`nexusd::tools`), the control API (`nexusd::control`), and the CLI all
/// need the exact same "don't silently answer from a stale index" guarantee;
/// this used to be duplicated in the first two and missing entirely from the
/// third, which is what let a project checked only via the CLI go cold
/// forever without ever self-healing. Callers should skip this for
/// `index_repository`/`Reindex`/`projects.reindex`/`delete_project` (already
/// unconditional, or about to make the entry moot) - checking first there
/// would just double the work or query a registry entry about to be deleted.
///
/// The `is_cold` check below is deliberately done twice - once here (cheap,
/// unlocked, so the common warm-project case never touches `REINDEX_LOCK` at
/// all) and once more inside the lock right before the reindex actually
/// runs. Without the second check, two tool calls racing against the same
/// freshly-cold project both see "cold" before either has reindexed, and
/// `REINDEX_LOCK` only serializes their two full rebuilds rather than
/// skipping the redundant one - on an embeddings-enabled project that's a
/// real duplicate embeddings-API spend, not just wasted CPU (see the GitHub
/// issue this fixes for the full race). The second check is what turns that
/// into ordinary double-checked locking: whichever caller loses the race to
/// acquire the lock finds the project already warm by the time it's their
/// turn and skips out instead of redoing the work.
pub fn touch_and_catchup(repo_path: &Path) {
    if is_cold(repo_path) {
        let _guard = REINDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if is_cold(repo_path) {
            let start = std::time::Instant::now();
            let success = index_project_locked(repo_path).is_ok();
            record_auto_reindex(repo_path, start.elapsed().as_millis() as u64, success);
        }
    }
    touch_queried(repo_path);
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
    // Canonicalize *before* the allowed_roots check, not after - matches the
    // fix in `queries.rs`'s `get_file_context`/`detect_changes` (issue #29).
    let repo_path = canonicalize_for_registry(repo_path)?;
    let repo_path = repo_path.as_path();
    require_path_allowed(&Paths::resolve(), repo_path)?;

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

/// Generous headroom over any legitimate use (this repo's own real
/// dogfooded `downtime` project, the largest measured so far, is ~119MB
/// post-Phase-28) while still bounding a decompression bomb - a crafted
/// `.zst` stream can trivially exceed 1000:1 compression ratios, so an
/// unbounded `zstd::stream::copy_decode` on an artifact from a compromised
/// or malicious teammate (exactly the "import a teammate's export"
/// workflow this feature exists for) could exhaust disk with a tiny input
/// file. See issue #31.
const MAX_DECOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Streams the decode in fixed-size chunks rather than `copy_decode`'s
/// single unbounded call, so the running total can be checked and the
/// partial output removed the moment it crosses `max_bytes`, instead of
/// only after disk is already exhausted. `max_bytes` is a parameter (rather
/// than reading `MAX_DECOMPRESSED_BYTES` directly) so tests can exercise
/// the rejection path with a small cap instead of needing gigabytes of
/// real data to trigger it.
fn decode_bounded(artifact: &Path, db_path: &Path, max_bytes: u64) -> Result<()> {
    use std::io::{Read, Write};

    let input = std::fs::File::open(artifact)?;
    let mut decoder = zstd::stream::read::Decoder::new(input)?;
    let mut output = std::fs::File::create(db_path)?;

    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = decoder.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            drop(output);
            let _ = std::fs::remove_file(db_path);
            bail!(
                "shared index artifact decompresses past the {} MiB safety cap - refusing \
                 (possible decompression bomb, or a corrupted/oversized artifact)",
                max_bytes / (1024 * 1024)
            );
        }
        output.write_all(&buf[..n])?;
    }
    Ok(())
}

/// Decompresses a shared artifact directly into place instead of walking the
/// tree-sitter pipeline - the whole point is skipping that cost. Registry is
/// updated the same way `index_project` does, using the imported DB's real
/// stats rather than trusting the artifact blindly.
pub fn import_project(repo_path: &Path) -> Result<IndexStats> {
    let repo_path = canonicalize_for_registry(repo_path)?;
    let repo_path = repo_path.as_path();

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

    decode_bounded(&artifact, &db_path, MAX_DECOMPRESSED_BYTES)?;

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

/// Resolves `repo_path` to an absolute, canonical path before it's allowed
/// anywhere near `record_indexed` - shared by `index_project_locked` and
/// `import_project`, the two entry points that persist `root_path` into the
/// registry. A relative path (e.g. `nexus reindex .`) stored verbatim is
/// exactly what let a real incident happen: the background watcher later
/// resolved a stored `"."` against *its own* cwd (the daemon's `$HOME`
/// under systemd), not the directory the CLI was run from, and recursively
/// watched the entire home tree until it exhausted the system's inotify
/// budget - see the GitHub issue this fixes. Canonicalizing once, here, at
/// write time means every reader of a registry `root_path` (the watcher,
/// the CLI, the GUI) can trust it's always absolute and resolved, with no
/// defensive re-canonicalizing needed at each read site.
fn canonicalize_for_registry(repo_path: &Path) -> Result<PathBuf> {
    repo_path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("repo_path does not exist: {}", repo_path.display()))
}

/// `pub(crate)` rather than private: `queries.rs`'s `get_file_context` and
/// `detect_changes` also take a caller-supplied `repo_path` and need the
/// same `allowed_roots` check `index_project`/`export_project`/
/// `import_project` already apply - see the GitHub issue this closes for
/// why leaving them unchecked made `allowed_roots` not actually mean what
/// its own doc comment says once you opted into it.
pub(crate) fn require_path_allowed(paths: &Paths, repo_path: &Path) -> Result<()> {
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
    // upsert() replaces the whole entry - carry the existing "last used"/
    // auto-reindex history forward rather than resetting it every time a
    // reindex happens to run.
    let existing = registry.projects.iter().find(|p| p.hash == hash).cloned();
    let last_queried_unix = existing.as_ref().map(|p| p.last_queried_unix).unwrap_or(0);
    let (
        auto_reindex_count,
        auto_reindex_fail_count,
        auto_reindex_total_ms,
        last_auto_reindex_ms,
        last_auto_reindex_unix,
    ) = existing
        .map(|p| {
            (
                p.auto_reindex_count,
                p.auto_reindex_fail_count,
                p.auto_reindex_total_ms,
                p.last_auto_reindex_ms,
                p.last_auto_reindex_unix,
            )
        })
        .unwrap_or_default();
    registry.upsert(ProjectEntry {
        root_path: repo_path.display().to_string(),
        hash,
        last_indexed_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        nodes,
        edges,
        last_queried_unix,
        auto_reindex_count,
        auto_reindex_fail_count,
        auto_reindex_total_ms,
        last_auto_reindex_ms,
        last_auto_reindex_unix,
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

#[cfg(test)]
mod canonicalize_for_registry_tests {
    use super::canonicalize_for_registry;
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_canonicalize_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Restores the original process cwd on drop, even if the test body
    /// between `enter` and the natural end of scope panics - without this,
    /// an `.unwrap()` failing partway through a cwd-mutating test would
    /// leave the process cwd poisoned for every subsequent test in this
    /// binary (process cwd is global, not per-thread), producing confusing
    /// order-dependent failures elsewhere. Caught in PR review.
    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl CwdGuard {
        fn enter(dir: &std::path::Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    #[test]
    fn relative_dot_resolves_to_the_current_directory_absolute_path() {
        let dir = temp_dir("relative_dot");
        let expected = dir.canonicalize().unwrap();

        // This is exactly the real incident: `nexus reindex .` run with cwd
        // set to the project root. `canonicalize_for_registry` has to turn
        // that bare "." into an absolute path *before* it's ever stored,
        // not leave it for a later reader (the watcher) to resolve against
        // a completely different cwd.
        //
        // NOTE: process cwd is global, not per-thread - if a future test
        // elsewhere in this crate starts depending on relative-path
        // resolution, it could flake if it happens to run concurrently
        // with this one under cargo test's default parallelism. No other
        // test currently does (grep for `current_dir` before adding one).
        let result = {
            let _guard = CwdGuard::enter(&dir);
            canonicalize_for_registry(std::path::Path::new("."))
        };

        assert_eq!(result.unwrap(), expected);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_absolute_path_round_trips() {
        let dir = temp_dir("already_absolute");
        let expected = dir.canonicalize().unwrap();
        assert_eq!(canonicalize_for_registry(&dir).unwrap(), expected);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonexistent_path_is_a_clear_error_not_a_silent_pass_through() {
        let missing = std::env::temp_dir().join("nexus_canonicalize_test_does_not_exist_12345");
        let err = canonicalize_for_registry(&missing).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }
}

/// Regression tests for issue #31: `import_project` had no decompression-
/// bomb protection at all.
#[cfg(test)]
mod decode_bounded_tests {
    use super::decode_bounded;
    use std::fs;
    use std::io::Write;

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_decode_bounded_test_{label}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn zstd_artifact(dir: &std::path::Path, raw: &[u8]) -> std::path::PathBuf {
        let path = dir.join("artifact.zst");
        let file = fs::File::create(&path).unwrap();
        let mut encoder = zstd::stream::write::Encoder::new(file, 3).unwrap();
        encoder.write_all(raw).unwrap();
        encoder.finish().unwrap();
        path
    }

    #[test]
    fn decompresses_normally_when_under_the_cap() {
        let dir = scratch_dir("under_cap");
        let raw = b"a small, ordinary shared-index artifact".repeat(10);
        let artifact = zstd_artifact(&dir, &raw);
        let out = dir.join("out.db");

        decode_bounded(&artifact, &out, 10_000).unwrap();

        assert_eq!(fs::read(&out).unwrap(), raw);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Highly repetitive input compresses to a tiny artifact but expands
    /// back to something well past a small cap - a stand-in for a crafted
    /// decompression bomb without needing gigabytes of real test data.
    #[test]
    fn refuses_and_cleans_up_when_decompressed_size_exceeds_the_cap() {
        let dir = scratch_dir("over_cap");
        let raw = vec![0u8; 1_000_000]; // compresses to well under 1KB
        let artifact = zstd_artifact(&dir, &raw);
        let out = dir.join("out.db");

        let err = decode_bounded(&artifact, &out, 1_000).unwrap_err();
        assert!(
            err.to_string().contains("safety cap"),
            "unexpected error: {err}"
        );
        assert!(
            !out.exists(),
            "partial output must be removed when the cap is exceeded, not left on disk"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
