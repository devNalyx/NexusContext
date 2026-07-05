use notify_debouncer_mini::new_debouncer;
use notify_debouncer_mini::notify::RecursiveMode;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_secs(2);
const REGISTRY_RESYNC_INTERVAL: Duration = Duration::from_secs(30);
/// Minimum gap between the *start* of consecutive auto-reindex attempts for
/// the same project, separate from the 2s debounce (which only coalesces
/// events within one burst, not across separate bursts). Without this, a
/// reindex that takes longer than a burst-to-burst gap - which embeddings
/// makes routine, since each project-wide reindex now makes real network
/// calls per batch of nodes instead of finishing in well under a second -
/// can lose the write lock race against its own next attempt: it fails with
/// "database is locked" after the 30s busy_timeout, and immediately
/// re-triggers, indefinitely, without ever getting a clear run at finishing.
const MIN_REINDEX_GAP: Duration = Duration::from_secs(45);

static WATCHED_COUNT: AtomicUsize = AtomicUsize::new(0);

/// For the GUI Dashboard / status.get - lets the control API report whether
/// auto-sync is actually watching anything without needing its own copy of
/// the registry-diffing logic.
pub fn watched_count() -> usize {
    WATCHED_COUNT.load(Ordering::Relaxed)
}

/// Background auto-sync: watches every registered project and reindexes it
/// (still a full rebuild - see the proposal's open risk on incremental
/// diffing) after a quiet period, instead of requiring a manual
/// reindex/projects.reindex call. `serve`-mode only; the per-session `mcp`
/// process has no business owning a persistent background thread.
pub fn spawn() {
    std::thread::spawn(|| {
        if let Err(err) = run() {
            tracing::warn!(error = %err, "file watcher stopped");
        }
    });
}

fn run() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = new_debouncer(DEBOUNCE, tx)?;

    let mut watched: HashSet<PathBuf> = HashSet::new();
    sync_watches(&mut debouncer, &mut watched);
    let mut last_sync = Instant::now();
    let mut last_attempt: HashMap<PathBuf, Instant> = HashMap::new();

    loop {
        match rx.recv_timeout(REGISTRY_RESYNC_INTERVAL) {
            Ok(Ok(events)) => {
                let mut to_reindex: HashSet<PathBuf> = HashSet::new();
                for event in events {
                    if is_noise(&event.path) {
                        continue;
                    }
                    if let Some(root) = watched.iter().find(|root| event.path.starts_with(root)) {
                        to_reindex.insert(root.clone());
                    }
                }
                for root in to_reindex {
                    if let Some(attempted_at) = last_attempt.get(&root) {
                        if attempted_at.elapsed() < MIN_REINDEX_GAP {
                            tracing::debug!(
                                project = %root.display(),
                                "file change detected, skipping - too soon after the last auto-reindex attempt"
                            );
                            continue;
                        }
                    }
                    tracing::info!(project = %root.display(), "file change detected, reindexing");
                    last_attempt.insert(root.clone(), Instant::now());
                    let reindex_start = Instant::now();
                    let success = match nexus_index::index_project(&root) {
                        Ok(_) => true,
                        Err(err) => {
                            tracing::warn!(project = %root.display(), error = %err, "auto-reindex failed");
                            false
                        }
                    };
                    nexus_index::record_auto_reindex(
                        &root,
                        reindex_start.elapsed().as_millis() as u64,
                        success,
                    );
                }
            }
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "file watcher event error");
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if last_sync.elapsed() > REGISTRY_RESYNC_INTERVAL {
            sync_watches(&mut debouncer, &mut watched);
            last_sync = std::time::Instant::now();
        }
    }
    Ok(())
}

/// Registered projects can change while the daemon is running (a new
/// project gets indexed via CLI/MCP/GUI at any time) - rather than wiring a
/// signal from every indexing call site into this thread, just re-read the
/// registry periodically and add/remove watches to match. Up to
/// `REGISTRY_RESYNC_INTERVAL` of lag before a brand-new project starts
/// being watched is an acceptable tradeoff for the simplicity.
fn sync_watches(
    debouncer: &mut notify_debouncer_mini::Debouncer<
        notify_debouncer_mini::notify::RecommendedWatcher,
    >,
    watched: &mut HashSet<PathBuf>,
) {
    let paths = nexus_core::Paths::resolve();
    let registry = nexus_core::Registry::load(&paths.registry_file());
    let warm_window_secs = nexus_core::Config::load(&paths.config_file())
        .map(|c| c.watcher.warm_window_secs)
        .unwrap_or_else(|_| nexus_core::WatcherConfig::default().warm_window_secs);
    let now = now_unix();
    // Cold projects (not queried within warm_window_secs) are excluded here
    // rather than filtered out of the reindex loop below - the whole point
    // is to stop paying for an active inotify watch on them, not just to
    // skip acting on events they'd otherwise generate.
    let current: HashSet<PathBuf> = registry
        .projects
        .iter()
        .filter(|p| p.is_warm(now, warm_window_secs))
        .map(|p| PathBuf::from(&p.root_path))
        .filter(|p| p.exists())
        .collect();

    for path in current.difference(watched) {
        if debouncer
            .watcher()
            .watch(path, RecursiveMode::Recursive)
            .is_ok()
        {
            tracing::info!(project = %path.display(), "watching for changes");
        }
    }
    for path in watched.difference(&current) {
        let _ = debouncer.watcher().unwatch(path);
    }
    *watched = current;
    WATCHED_COUNT.store(watched.len(), Ordering::Relaxed);
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Simple path-component denylist rather than full .gitignore semantics in
/// the watcher itself - the ingestion walk already respects .gitignore for
/// what gets *indexed*; this just avoids triggering a reindex storm from
/// noisy directories that change constantly during a build.
fn is_noise(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git") | Some("target") | Some("node_modules") | Some(".nexuscontext")
        )
    })
}
