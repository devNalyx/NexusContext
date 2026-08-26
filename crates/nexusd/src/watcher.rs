use notify_debouncer_mini::new_debouncer;
use notify_debouncer_mini::notify::RecursiveMode;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_secs(2);
const REGISTRY_RESYNC_INTERVAL: Duration = Duration::from_secs(30);
/// Minimum gap after finishing one auto-reindex attempt before starting the
/// next one for the same project, separate from the 2s debounce (which only
/// coalesces events within one burst, not across separate bursts). Recorded
/// *after* an attempt finishes (indexing + re-establishing the watch), not
/// before it starts - see the two things that land in this window:
///
/// 1. Without a gap at all, a reindex slower than the burst-to-burst rate
///    can lose the write lock race against its own next attempt: it fails
///    with "database is locked" after the 30s busy_timeout, and immediately
///    re-triggers, indefinitely.
/// 2. Re-establishing the watch after an attempt isn't free either: notify's
///    Linux backend subscribes to IN_OPEN (not just writes - see
///    inotify.rs's watchmask), and its recursive watch setup walks the whole
///    tree opening every subdirectory to register a watch on it. Since the
///    root's own watch is registered before that walk reaches its children,
///    those subdirectory opens land right back on the watch just
///    re-created - a burst of self-generated "changed" events at exactly
///    the moment watching resumes. On a large project (thousands of nodes,
///    many subdirectories) that walk measurably outlasts a short gap - 45s
///    wasn't enough and let a straggler in ~85s after the previous attempt
///    finished, which read as a "new" edit and re-triggered indefinitely.
///    3 minutes gives real margin over any plausible walk duration; a
///    genuine edit landing in that window isn't lost, just picked up on the
///    project's next eligible attempt rather than instantly.
const MIN_REINDEX_GAP: Duration = Duration::from_secs(180);

/// How long an evicted project is excluded from being re-admitted, even
/// though it's still warm and the eviction that just freed room for it
/// would otherwise let `sync_watches`/`plan_admission` add it right back.
/// Without this, sustained real watch pressure (an estimate that
/// persistently undercounts actual usage, or another app on the box also
/// consuming inotify watches) flips the same project in and out on every
/// `REGISTRY_RESYNC_INTERVAL`: evict -> next sync re-admits it (the
/// eviction freed exactly the room it needs) -> pressure returns -> evict
/// again. Caught in PR review, not a hypothetical - found while looking at
/// exactly this eviction path. Long enough to actually break that cycle,
/// short enough that a since-resolved pressure spike doesn't leave a
/// project un-watched far longer than the pressure that caused it.
const EVICTION_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// Self-imposed ceiling on total inotify watches this daemon will hold, as
/// a fraction of what the OS reports available - deliberately well under
/// 100%, since this daemon isn't the only thing on the machine that needs
/// watches (editors, Obsidian, sync tools all compete for the same
/// per-user budget). The real incident this closes: a single project
/// (root_path stored as a relative "." - see `canonicalize_for_registry`
/// in nexus-index's project.rs, fixed alongside this) got recursively
/// watched against the daemon's own cwd, which resolved to the whole
/// `$HOME`, consuming the *entire* system inotify budget and starving
/// every other app - Obsidian included - of watches. A cap that still
/// allowed using 100% of the system limit would let the same class of
/// problem recur with different numbers; leaving real headroom is the
/// actual fix, not just a tighter version of the same all-or-nothing
/// posture.
const WATCH_BUDGET_FRACTION: f64 = 0.5;

/// Used when the real system limit can't be read (non-Linux, no `/proc`,
/// a permission issue in a sandboxed environment) - deliberately
/// conservative rather than the commonly-much-higher real Linux default
/// (65536), so an unknown limit errs toward under-watching, not
/// over-watching.
const FALLBACK_SYSTEM_WATCH_LIMIT: usize = 8192;

/// Floor under `WATCH_BUDGET_FRACTION` so an unusually low or
/// misconfigured system limit doesn't leave this daemon with room for
/// zero projects - one modest project should always fit somewhere.
const MIN_WATCH_BUDGET: usize = 256;

static WATCHED_COUNT: AtomicUsize = AtomicUsize::new(0);
static WATCH_ESTIMATE_USED: AtomicUsize = AtomicUsize::new(0);
/// How many times a project has been skipped (too big to fit alone) or
/// evicted (bumped to make room for a more recently used one) since the
/// daemon started. Zero for the overwhelming majority of setups; a
/// non-zero count is the "loud" signal that `fs.inotify.max_user_watches`
/// is actually constraining real usage on this machine, worth surfacing
/// rather than left buried in a log file nobody reads until something else
/// (Obsidian, an editor) breaks first - see `status.get`'s `watch_status`.
static WATCH_PRESSURE_EVENTS: AtomicUsize = AtomicUsize::new(0);

/// For the GUI Dashboard / status.get - lets the control API report
/// auto-sync watch state without needing its own copy of the
/// registry-diffing/budget logic.
pub struct WatchStatus {
    pub watched_projects: usize,
    pub estimated_watches_used: usize,
    pub estimated_watches_budget: usize,
    pub pressure_events: usize,
}

pub fn watch_status() -> WatchStatus {
    WatchStatus {
        watched_projects: WATCHED_COUNT.load(Ordering::Relaxed),
        estimated_watches_used: WATCH_ESTIMATE_USED.load(Ordering::Relaxed),
        estimated_watches_budget: watch_budget(),
        pressure_events: WATCH_PRESSURE_EVENTS.load(Ordering::Relaxed),
    }
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

    // Report the constraint once at startup rather than only discovering
    // it via a WARN buried in the log the first time something doesn't
    // fit - see the GitHub issue this closes for why "silently eat it"
    // wasn't good enough.
    tracing::info!(
        budget = watch_budget(),
        "inotify watch budget for auto-sync (self-imposed fraction of the OS limit)"
    );

    let mut watched: HashMap<PathBuf, usize> = HashMap::new();
    // When a project was last evicted under watch pressure - see
    // EVICTION_COOLDOWN's doc comment for why this exists at all.
    let mut evicted_at: HashMap<PathBuf, Instant> = HashMap::new();
    sync_watches(&mut debouncer, &mut watched, &mut evicted_at);
    let mut last_sync = Instant::now();
    let mut last_attempt: HashMap<PathBuf, Instant> = HashMap::new();
    let mut content_signatures: HashMap<PathBuf, u64> = HashMap::new();

    loop {
        match rx.recv_timeout(REGISTRY_RESYNC_INTERVAL) {
            Ok(message) => {
                let mut to_reindex: HashSet<PathBuf> = HashSet::new();
                let mut hit_watch_limit = collect_dirty_roots(message, &watched, &mut to_reindex);

                // A reindex can take far longer than events arrive on a large
                // project, so while one is running, every separate debounced burst that
                // lands for the same project (or another) queues up in this
                // channel independently, since it's unbounded and nothing else
                // is draining it. Left alone, N queued bursts for one project
                // become N full serial reindexes played back one after
                // another long after the real edits happened, each ~11
                // minutes apart on this project's numbers, with the "file
                // change" in the log being nothing more than the previous
                // reindex finally finishing. Draining everything already
                // queued and collapsing it into one pass fixes that: a full
                // reindex started after the last of them already captures
                // every change that arrived before it, so only one is needed.
                while let Ok(message) = rx.try_recv() {
                    hit_watch_limit |= collect_dirty_roots(message, &watched, &mut to_reindex);
                }

                if hit_watch_limit {
                    // The proactive budget in sync_watches already tries to
                    // avoid this - a live ENOSPC despite that means the
                    // per-project estimate undercounted (a directory grew
                    // after being estimated, symlinks, a race) or something
                    // else on the system is also consuming watches. Relieve
                    // pressure immediately rather than waiting up to
                    // REGISTRY_RESYNC_INTERVAL and leaving every other app
                    // on the machine still starved in the meantime.
                    evict_least_recently_used(&mut debouncer, &mut watched, &mut evicted_at);
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
                    // Cheap check before paying for an expensive reindex:
                    // notify firing on opens (not just writes, see
                    // MIN_REINDEX_GAP's doc comment) means any read-only
                    // tool poking around the tree - git status, cargo
                    // build, an editor, another diagnostic command - can
                    // wake this loop up with nothing having actually
                    // changed. Comparing a path+size+mtime signature over
                    // exactly the files indexing would touch catches that
                    // before it costs a real reindex; only a genuine
                    // difference proceeds past this point.
                    let signature = nexus_index::content_signature(&root);
                    if content_signatures.get(&root) == Some(&signature) {
                        tracing::debug!(
                            project = %root.display(),
                            "file change detected, but indexed content is unchanged - skipping"
                        );
                        continue;
                    }

                    tracing::info!(project = %root.display(), "file change detected, reindexing");

                    // Indexing itself opens and reads every file in the tree
                    // to parse it - and notify's Linux backend subscribes to
                    // IN_OPEN, not just writes (see inotify.rs's watchmask),
                    // so those reads are otherwise indistinguishable from a
                    // real edit once debounced. Unwatching for the duration
                    // avoids that, but re-watching afterward isn't free
                    // either: notify's recursive watch setup walks the whole
                    // tree opening every subdirectory to register a watch on
                    // it, and since the root's own watch is registered first
                    // (before the walk reaches its children), those
                    // subdirectory opens land right back on the watch we
                    // just re-created - a burst of self-generated "changed"
                    // events at the exact moment we resume watching.
                    let _ = debouncer.watcher().unwatch(&root);
                    let reindex_start = Instant::now();
                    let success = match nexus_index::index_project(&root) {
                        Ok(_) => true,
                        Err(err) => {
                            tracing::warn!(project = %root.display(), error = %err, "auto-reindex failed");
                            false
                        }
                    };
                    let estimate = watched.get(&root).copied().unwrap_or_else(|| {
                        nexus_index::estimate_watch_count(&root, watch_budget())
                    });
                    // Deliberately doesn't check EVICTION_COOLDOWN/
                    // is_admission_candidate here, unlike sync_watches -
                    // cooldown governs re-*admission* of a project nothing
                    // has actually touched since it was evicted; a project
                    // that just got reindexed because its files genuinely
                    // changed has earned a fresh watch regardless of
                    // whether it happened to be evicted moments earlier in
                    // the same batch. Self-limiting either way: under
                    // persistent real pressure the watch() call below
                    // fails (not recorded in `watched`), and the cooldown
                    // still holds for sync_watches' own admission path
                    // afterward.
                    if debouncer
                        .watcher()
                        .watch(&root, RecursiveMode::Recursive)
                        .is_ok()
                    {
                        watched.insert(root.clone(), estimate);
                    } else {
                        // Let the next periodic sync_watches retry rather than
                        // silently giving up on this project forever - it
                        // only recognizes a project as needing a (re)watch
                        // when it's missing from `watched`.
                        watched.remove(&root);
                    }
                    WATCHED_COUNT.store(watched.len(), Ordering::Relaxed);
                    WATCH_ESTIMATE_USED.store(watched.values().sum(), Ordering::Relaxed);
                    // Recorded now, after re-watching, rather than before
                    // indexing started - so MIN_REINDEX_GAP is measured from
                    // "we just finished handling this project" rather than
                    // "we started 11 minutes ago". That's what actually
                    // absorbs the re-watch's own self-generated burst above:
                    // it lands within the gap of *this* attempt and gets
                    // skipped by the check at the top of this loop, instead
                    // of always passing because 11 minutes had already
                    // elapsed since a start-of-attempt timestamp.
                    last_attempt.insert(root.clone(), Instant::now());
                    // The pre-reindex signature, not a freshly-recomputed
                    // one - this is "what we just handled", so the next
                    // wake-up only proceeds if something differs from it.
                    content_signatures.insert(root.clone(), signature);
                    nexus_index::record_auto_reindex(
                        &root,
                        reindex_start.elapsed().as_millis() as u64,
                        success,
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if last_sync.elapsed() > REGISTRY_RESYNC_INTERVAL {
            sync_watches(&mut debouncer, &mut watched, &mut evicted_at);
            last_sync = std::time::Instant::now();
        }
    }
    Ok(())
}

/// Turns one channel message (a debounced batch of events, or a watcher
/// error) into noise-filtered dirty project roots, merged into `to_reindex`.
/// Factored out so both the blocking `recv_timeout` and the non-blocking
/// drain loop in `run` apply the same filtering to every message they see.
///
/// Returns whether this message signaled the OS's inotify watch limit was
/// hit (`notify::ErrorKind::MaxFilesWatch`) - the caller uses that to
/// trigger immediate LRU eviction instead of just logging and leaving
/// every other app on the machine starved until the next periodic resync.
fn collect_dirty_roots(
    message: notify_debouncer_mini::DebounceEventResult,
    watched: &HashMap<PathBuf, usize>,
    to_reindex: &mut HashSet<PathBuf>,
) -> bool {
    match message {
        Ok(events) => {
            for event in events {
                if is_noise(&event.path) {
                    continue;
                }
                if let Some(root) = watched.keys().find(|root| event.path.starts_with(root)) {
                    to_reindex.insert(root.clone());
                }
            }
            false
        }
        Err(err) => {
            let hit_watch_limit = matches!(
                err.kind,
                notify_debouncer_mini::notify::ErrorKind::MaxFilesWatch
            );
            if hit_watch_limit {
                tracing::warn!(
                    error = %err,
                    "OS inotify watch limit reached despite this daemon's own self-imposed budget \
                     - evicting the least-recently-used watched project to relieve pressure"
                );
            } else {
                tracing::warn!(error = %err, "file watcher event error");
            }
            hit_watch_limit
        }
    }
}

/// Unwatches the single least-recently-queried currently-watched project,
/// freeing up its estimated watch count. Used reactively, from the main
/// event loop, when a live ENOSPC gets through despite `sync_watches`'
/// proactive budget - re-reads the registry itself (cheap, rare) rather
/// than threading `last_queried_unix` data through from `sync_watches`,
/// since this can fire between syncs.
fn evict_least_recently_used(
    debouncer: &mut notify_debouncer_mini::Debouncer<
        notify_debouncer_mini::notify::RecommendedWatcher,
    >,
    watched: &mut HashMap<PathBuf, usize>,
    evicted_at: &mut HashMap<PathBuf, Instant>,
) {
    let paths = nexus_core::Paths::resolve();
    let registry = nexus_core::Registry::load(&paths.registry_file());
    let last_queried: HashMap<PathBuf, u64> = registry
        .projects
        .iter()
        .map(|p| (PathBuf::from(&p.root_path), p.last_queried_unix))
        .collect();

    let Some(victim) = watched
        .keys()
        .min_by_key(|p| last_queried.get(*p).copied().unwrap_or(0))
        .cloned()
    else {
        return; // nothing watched, nothing to evict
    };

    tracing::warn!(
        evicted = %victim.display(),
        "evicted from the auto-sync watch set to relieve inotify pressure - its structural index \
         is untouched, it just won't auto-reindex on file change until it's queried again"
    );
    let _ = debouncer.watcher().unwatch(&victim);
    watched.remove(&victim);
    // Starts EVICTION_COOLDOWN - without this, the very next sync_watches
    // would just re-admit `victim` (the eviction freed exactly the room it
    // needs) and, under sustained pressure, flip it in and out every
    // REGISTRY_RESYNC_INTERVAL.
    evicted_at.insert(victim, Instant::now());
    WATCHED_COUNT.store(watched.len(), Ordering::Relaxed);
    WATCH_ESTIMATE_USED.store(watched.values().sum(), Ordering::Relaxed);
    WATCH_PRESSURE_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// Whether a warm, not-currently-watched project should be considered a
/// candidate for (re-)admission right now - `false` if it's still within
/// its post-eviction cooldown. Pulled out of `sync_watches`' `to_add`
/// filter into its own function so this decision is unit-testable without
/// a live watcher, the same reasoning `plan_admission` above already
/// applies to the eviction-ordering decision. Governs `sync_watches`'
/// admission path only - the `run` loop's reindex-triggered re-watch
/// doesn't check this (see its own comment for why that's fine).
fn is_admission_candidate(path: &Path, evicted_at: &HashMap<PathBuf, Instant>) -> bool {
    !evicted_at.contains_key(path)
}

/// Registered projects can change while the daemon is running (a new
/// project gets indexed via CLI/MCP/GUI at any time) - rather than wiring a
/// signal from every indexing call site into this thread, just re-read the
/// registry periodically and add/remove watches to match. Up to
/// `REGISTRY_RESYNC_INTERVAL` of lag before a brand-new project starts
/// being watched is an acceptable tradeoff for the simplicity.
///
/// Also self-heals any warm registry entry whose stored `root_path` isn't
/// already canonical (see `heal_noncanonical_root_paths`) - re-canonicalizes
/// it and persists the correction, rather than ever watching whatever the
/// daemon's own cwd happens to resolve a relative path against. Entries
/// written before `nexus_index::project::canonicalize_for_registry`
/// existed (or any other drift) are exactly what this catches - see the
/// GitHub issue this closes for the real incident that motivated it.
///
/// Never lets total watched-project usage exceed `watch_budget()`: adding
/// a newly-warm project evicts the least-recently-queried currently-watched
/// project(s) first if needed to make room, and skips the new project
/// entirely (loud warning, no partial watch - notify has no "watch this
/// tree except its biggest subdirectory" mode) if its own estimated watch
/// count alone exceeds the whole budget.
fn sync_watches(
    debouncer: &mut notify_debouncer_mini::Debouncer<
        notify_debouncer_mini::notify::RecommendedWatcher,
    >,
    watched: &mut HashMap<PathBuf, usize>,
    evicted_at: &mut HashMap<PathBuf, Instant>,
) {
    let paths = nexus_core::Paths::resolve();
    let mut registry = nexus_core::Registry::load(&paths.registry_file());
    let warm_window_secs = nexus_core::Config::load(&paths.config_file())
        .map(|c| c.watcher.warm_window_secs)
        .unwrap_or_else(|_| nexus_core::WatcherConfig::default().warm_window_secs);
    let now = now_unix();
    let budget = watch_budget();

    if heal_noncanonical_root_paths(&mut registry.projects, now, warm_window_secs) {
        let _ = registry.save(&paths.registry_file());
    }

    // Cheap housekeeping: drop cooldown entries that have already expired,
    // both so the map doesn't grow unbounded over the daemon's lifetime
    // and so the filter below only has to check "is this key present".
    evicted_at.retain(|_, at| at.elapsed() < EVICTION_COOLDOWN);

    let mut last_queried: HashMap<PathBuf, u64> = HashMap::new();
    let mut current: HashSet<PathBuf> = HashSet::new();
    for entry in &registry.projects {
        if !entry.is_warm(now, warm_window_secs) {
            continue;
        }
        let path = PathBuf::from(&entry.root_path);
        if !path.exists() {
            continue;
        }
        last_queried.insert(path.clone(), entry.last_queried_unix);
        current.insert(path);
    }

    // Stop watching whatever went cold or was deregistered.
    let to_drop: Vec<PathBuf> = watched
        .keys()
        .filter(|p| !current.contains(*p))
        .cloned()
        .collect();
    for path in to_drop {
        let _ = debouncer.watcher().unwatch(&path);
        watched.remove(&path);
    }

    // Watch whatever's newly warm, in a deterministic order (sorted by
    // path) so eviction/skip decisions aren't dependent on HashSet
    // iteration order run to run. A path still in its post-eviction
    // cooldown is held back even though it's warm and missing from
    // `watched` - see EVICTION_COOLDOWN's doc comment for why re-admitting
    // it immediately would just flip it in and out under sustained
    // pressure.
    let mut to_add: Vec<PathBuf> = current
        .iter()
        .filter(|p| !watched.contains_key(*p))
        .filter(|p| {
            let admit = is_admission_candidate(p, evicted_at);
            if !admit {
                tracing::debug!(
                    project = %p.display(),
                    "still in post-eviction cooldown, not re-admitting yet"
                );
            }
            admit
        })
        .cloned()
        .collect();
    to_add.sort();

    for path in to_add {
        let estimate = nexus_index::estimate_watch_count(&path, budget);

        let Some(to_evict) = plan_admission(estimate, budget, watched, &last_queried) else {
            tracing::warn!(
                project = %path.display(),
                estimated_watches = estimate,
                budget,
                "project needs more inotify watches than this daemon's entire self-imposed budget \
                 - not auto-watching it. Structural tools (search_graph, trace_call_path, \
                 get_architecture, search_code, query_planner, etc.) still work fully; this \
                 project just won't auto-reindex on file change - reindex manually after edits, \
                 or raise fs.inotify.max_user_watches if you want it auto-watched."
            );
            WATCH_PRESSURE_EVENTS.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        for victim in to_evict {
            tracing::warn!(
                evicted = %victim.display(),
                making_room_for = %path.display(),
                budget,
                "inotify watch budget full - evicting the least-recently-used watched project to \
                 make room for a more recently used one. The evicted project's structural index is \
                 untouched; it just won't auto-reindex on file change until it's queried again."
            );
            let _ = debouncer.watcher().unwatch(&victim);
            watched.remove(&victim);
            evicted_at.insert(victim, Instant::now());
            WATCH_PRESSURE_EVENTS.fetch_add(1, Ordering::Relaxed);
        }

        match debouncer.watcher().watch(&path, RecursiveMode::Recursive) {
            Ok(()) => {
                tracing::info!(
                    project = %path.display(),
                    estimated_watches = estimate,
                    "watching for changes"
                );
                watched.insert(path, estimate);
            }
            Err(err) => {
                // Previously silent - a failed watch() call here meant a
                // project quietly never got auto-reindexed on file change,
                // with no signal anywhere that it happened. Now logged,
                // and critically *not* recorded in `watched`: recording it
                // regardless of success used to permanently block ever
                // retrying, since the next sync only considers paths
                // *missing* from `watched` as candidates to (re-)add.
                tracing::warn!(
                    project = %path.display(),
                    error = %err,
                    "failed to watch project for changes"
                );
            }
        }
    }

    WATCHED_COUNT.store(watched.len(), Ordering::Relaxed);
    WATCH_ESTIMATE_USED.store(watched.values().sum(), Ordering::Relaxed);
}

/// Pure admission decision, separated from the actual `debouncer.watcher()`
/// calls so it's unit-testable without a live watcher: can a project
/// needing `estimate` watches fit under `budget`, given `currently_watched`
/// projects and their `last_queried` times as eviction candidates?
///
/// Returns `Some(paths_to_evict)` (in eviction order, oldest-queried
/// first, possibly empty if it already fits) if the new project can be
/// admitted, or `None` if it can't fit even after evicting every currently
/// watched project - the "skip this one entirely" case, since notify has
/// no way to partially watch a tree.
fn plan_admission(
    estimate: usize,
    budget: usize,
    currently_watched: &HashMap<PathBuf, usize>,
    last_queried: &HashMap<PathBuf, u64>,
) -> Option<Vec<PathBuf>> {
    if estimate > budget {
        return None;
    }

    let mut candidates: Vec<(PathBuf, usize, u64)> = currently_watched
        .iter()
        .map(|(path, weight)| {
            (
                path.clone(),
                *weight,
                last_queried.get(path).copied().unwrap_or(0),
            )
        })
        .collect();
    // Least-recently-queried first - and by path as a tiebreaker, so this
    // is fully deterministic (not dependent on HashMap iteration order)
    // when two candidates share a last_queried_unix.
    candidates.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));

    let mut used: usize = currently_watched.values().sum();
    let mut to_evict = Vec::new();
    for (path, weight, _) in candidates {
        if estimate <= budget.saturating_sub(used) {
            break;
        }
        used = used.saturating_sub(weight);
        to_evict.push(path);
    }

    Some(to_evict)
}

/// Corrects any warm registry entry whose `root_path` isn't already the
/// canonical form of itself. True for every entry written after
/// `canonicalize_for_registry` started enforcing this at write time, but a
/// registry from before that fix (or any other drift - a moved mount
/// point, a symlink that changed) could still have a relative or
/// non-canonical path stored. Returns whether anything changed, so the
/// caller only pays for a `registry.save()` when there's actually
/// something to persist.
///
/// Only touches *warm* entries - a cold project isn't about to be watched
/// anyway, so there's no need to canonicalize (and potentially fail to,
/// e.g. a project deleted from disk) something nothing is about to read.
fn heal_noncanonical_root_paths(
    projects: &mut [nexus_core::ProjectEntry],
    now: u64,
    warm_window_secs: u64,
) -> bool {
    let mut changed = false;
    for entry in projects.iter_mut() {
        if !entry.is_warm(now, warm_window_secs) {
            continue;
        }
        let stored = PathBuf::from(&entry.root_path);
        let Ok(canonical) = stored.canonicalize() else {
            continue; // doesn't exist right now - sync_watches' own
                      // `path.exists()` filter already handles that
                      // downstream; nothing to correct here.
        };
        if canonical != stored {
            tracing::warn!(
                stored = %stored.display(),
                canonical = %canonical.display(),
                "registry entry had a non-canonical root_path - correcting it"
            );
            entry.root_path = canonical.display().to_string();
            changed = true;
        }
    }
    changed
}

fn compute_budget(system_limit: usize) -> usize {
    ((system_limit as f64 * WATCH_BUDGET_FRACTION) as usize).max(MIN_WATCH_BUDGET)
}

/// Reads `/proc/sys/fs/inotify/max_user_watches` (Linux-specific, matching
/// this project's existing Ubuntu/GNOME-first scope - notify's inotify
/// backend is Linux-only regardless) and applies `compute_budget`'s safety
/// fraction. Falls back to `FALLBACK_SYSTEM_WATCH_LIMIT` if the file can't
/// be read or parsed (non-Linux, permissions, a sandboxed environment
/// without `/proc`).
fn watch_budget() -> usize {
    let system_limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(FALLBACK_SYSTEM_WATCH_LIMIT);
    compute_budget(system_limit)
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

#[cfg(test)]
mod is_admission_candidate_tests {
    use super::*;

    #[test]
    fn a_path_never_evicted_is_a_candidate() {
        let evicted_at = HashMap::new();
        assert!(is_admission_candidate(
            &PathBuf::from("/projects/a"),
            &evicted_at
        ));
    }

    #[test]
    fn a_recently_evicted_path_is_not_a_candidate() {
        let evicted_at = HashMap::from([(PathBuf::from("/projects/a"), Instant::now())]);
        assert!(!is_admission_candidate(
            &PathBuf::from("/projects/a"),
            &evicted_at
        ));
    }

    #[test]
    fn only_the_evicted_path_is_excluded_not_every_path() {
        let evicted_at = HashMap::from([(PathBuf::from("/projects/a"), Instant::now())]);
        assert!(is_admission_candidate(
            &PathBuf::from("/projects/b"),
            &evicted_at
        ));
    }
}

#[cfg(test)]
mod plan_admission_tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/projects/{name}"))
    }

    #[test]
    fn fits_with_no_eviction_when_theres_already_enough_room() {
        let watched = HashMap::from([(path("a"), 10)]);
        let last_queried = HashMap::from([(path("a"), 100)]);
        let plan = plan_admission(5, 20, &watched, &last_queried).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn evicts_the_single_least_recently_queried_project_to_make_room() {
        let watched = HashMap::from([(path("old"), 10), (path("new"), 10)]);
        let last_queried = HashMap::from([(path("old"), 100), (path("new"), 500)]);
        // budget 15, both watched (20 used, already over): evicting just
        // "old" (the least recently queried) drops used to 10, leaving 5
        // free - exactly enough for an incoming project needing 5.
        let plan = plan_admission(5, 15, &watched, &last_queried).unwrap();
        assert_eq!(plan, vec![path("old")]);
    }

    #[test]
    fn evicts_multiple_projects_oldest_first_until_it_fits() {
        let watched = HashMap::from([
            (path("oldest"), 5),
            (path("middle"), 5),
            (path("newest"), 5),
        ]);
        let last_queried = HashMap::from([
            (path("oldest"), 100),
            (path("middle"), 200),
            (path("newest"), 300),
        ]);
        // budget 11, all three watched (15 used, already over): evicting
        // just "oldest" only frees it to 10 used / 1 free - not enough for
        // an incoming project needing 6, so "middle" has to go too,
        // leaving just "newest" (5 used / 6 free, exactly enough).
        let plan = plan_admission(6, 11, &watched, &last_queried).unwrap();
        assert_eq!(plan, vec![path("oldest"), path("middle")]);
    }

    #[test]
    fn returns_none_when_it_cannot_fit_even_after_evicting_everything() {
        let watched = HashMap::from([(path("a"), 5)]);
        let last_queried = HashMap::from([(path("a"), 100)]);
        // The incoming project alone (30) is bigger than the whole budget
        // (20) - no amount of eviction helps; this is the "skip it
        // entirely" case, not an eviction case.
        assert!(plan_admission(30, 20, &watched, &last_queried).is_none());
    }

    #[test]
    fn empty_watched_set_with_room_needs_no_eviction() {
        let watched = HashMap::new();
        let last_queried = HashMap::new();
        let plan = plan_admission(5, 20, &watched, &last_queried).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn ties_in_last_queried_break_by_path_for_determinism() {
        let watched = HashMap::from([(path("b"), 10), (path("a"), 10)]);
        let last_queried = HashMap::from([(path("b"), 100), (path("a"), 100)]);
        // Same shape as the single-eviction test above, but with a and b
        // tied on last_queried - "a" sorts before "b" as the tiebreak, so
        // "a" is the one evicted even though timestamp alone can't decide.
        let plan = plan_admission(5, 15, &watched, &last_queried).unwrap();
        assert_eq!(plan, vec![path("a")]);
    }
}

#[cfg(test)]
mod compute_budget_tests {
    use super::*;

    #[test]
    fn takes_the_configured_fraction_of_the_system_limit() {
        assert_eq!(
            compute_budget(65536),
            (65536.0 * WATCH_BUDGET_FRACTION) as usize
        );
    }

    #[test]
    fn never_goes_below_the_floor_for_a_low_system_limit() {
        assert_eq!(compute_budget(100), MIN_WATCH_BUDGET);
    }

    #[test]
    fn floor_and_fraction_agree_at_the_crossover_point() {
        // Just under the point where the fraction alone would drop below
        // the floor - the floor must still win.
        let crossover = (MIN_WATCH_BUDGET as f64 / WATCH_BUDGET_FRACTION) as usize;
        assert!(compute_budget(crossover - 1) >= MIN_WATCH_BUDGET);
    }
}

#[cfg(test)]
mod heal_noncanonical_root_paths_tests {
    use super::*;
    use nexus_core::ProjectEntry;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nexus_watcher_heal_test_{name}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Restores the original process cwd on drop, even if the test body
    /// between `enter` and the natural end of scope panics - see
    /// nexus-index's `project.rs` `canonicalize_for_registry_tests` module
    /// for the identical helper and the full reasoning (duplicated here
    /// rather than shared, since it's test-only and these are two
    /// different crates). Caught in PR review.
    struct CwdGuard {
        original: PathBuf,
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

    fn entry(root_path: &str, last_queried_unix: u64) -> ProjectEntry {
        ProjectEntry {
            root_path: root_path.to_string(),
            hash: "abc123".to_string(),
            last_indexed_unix: 1_000_000,
            nodes: 1,
            edges: 0,
            last_queried_unix,
            auto_reindex_count: 0,
            auto_reindex_fail_count: 0,
            auto_reindex_total_ms: 0,
            last_auto_reindex_ms: 0,
            last_auto_reindex_unix: 0,
        }
    }

    #[test]
    fn corrects_a_relative_root_path_on_a_warm_entry() {
        let dir = temp_dir("relative");
        // Process cwd is global, not per-thread - this is the only test in
        // this file that mutates it (see the cold-entry test's comment for
        // why the other seemingly-similar test doesn't need to), so cargo
        // test's default parallelism can't race two cwd mutations against
        // each other here. Keep it that way if adding more cwd-dependent
        // tests to this module. CwdGuard restores cwd on drop even if an
        // assertion below panics.
        let now = 1_000_000;
        let changed = {
            let _guard = CwdGuard::enter(&dir);
            let mut projects = vec![entry(".", now)]; // the exact real-incident shape
            let changed = heal_noncanonical_root_paths(&mut projects, now, 6 * 3600);
            assert_eq!(
                projects[0].root_path,
                dir.canonicalize().unwrap().display().to_string()
            );
            changed
        };

        assert!(changed);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaves_an_already_canonical_root_path_untouched() {
        let dir = temp_dir("already_canonical");
        let canonical = dir.canonicalize().unwrap().display().to_string();
        let now = 1_000_000;
        let mut projects = vec![entry(&canonical, now)];
        let changed = heal_noncanonical_root_paths(&mut projects, now, 6 * 3600);
        assert!(!changed);
        assert_eq!(projects[0].root_path, canonical);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_touch_a_cold_entry_even_if_relative() {
        // No cwd manipulation needed here (unlike the warm-entry test
        // above): a cold entry short-circuits before canonicalize() is
        // ever called, so what "." would even resolve to is irrelevant -
        // that's the exact thing this test proves. Also keeps this test
        // from racing the warm-entry test's cwd mutation under cargo
        // test's default thread parallelism (process cwd is global, not
        // per-thread) - only one test in this file needs to touch it.
        //
        // last_queried_unix = 0 -> never queried -> cold per ProjectEntry::is_warm.
        let mut projects = vec![entry(".", 0)];
        let changed = heal_noncanonical_root_paths(&mut projects, 1_000_000, 6 * 3600);

        assert!(!changed);
        assert_eq!(projects[0].root_path, ".");
    }

    #[test]
    fn a_path_that_no_longer_exists_is_left_alone_not_errored() {
        let now = 1_000_000;
        let mut projects = vec![entry("/nonexistent/nexus-watcher-heal-test-path", now)];
        let changed = heal_noncanonical_root_paths(&mut projects, now, 6 * 3600);
        assert!(!changed);
    }
}
