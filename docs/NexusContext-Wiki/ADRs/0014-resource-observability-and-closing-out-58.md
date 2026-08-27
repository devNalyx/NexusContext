# 0014. Resource observability closes out issue #58, with two items explicitly deferred

Status: Accepted (amended)
Date: 2026-08-27 (amended 2026-08-27)

## Amendment: cooperative reindex cancellation on supersession

The original decision below left "background-work cancellation for a
superseded reindex request" as a real, deliberately-deferred gap - full
mid-file cancellation was correctly judged too big a lift to fold into this
pass. A dedicated follow-up traced through exactly what happens today when
more file changes land in a project *while* its reindex is already running,
concretely (not by inspection alone):

- `nexusd::watcher::run`'s main loop is single-threaded and blocks for the
  whole duration of its `nexus_index::index_project` call - it cannot drain
  its own channel or notice anything while that call is in flight.
- `notify_debouncer_mini`'s debounce thread, however, is separate and keeps
  running the whole time: it still sends every new debounced batch onto the
  bounded channel (`WATCHER_CHANNEL_BOUND`), which simply queues (or, once
  full, applies backpressure) rather than dropping anything.
- Once `index_project` returns, `run`'s loop resumes, drains everything
  that queued up during the reindex in one pass (the same coalescing this
  ADR's Decision section already described), computes a fresh
  `content_signature`, and - if it differs from the signature captured
  before the just-finished reindex - triggers exactly one follow-up
  reindex, subject only to `MIN_REINDEX_GAP`.

**Conclusion: this is the wasted-work case, not the data-loss case.**
Nothing is silently dropped - every change made during an in-flight reindex
is guaranteed to be captured by a follow-up pass. The real cost is that the
in-flight pass runs to completion re-processing files that were already
known to be stale, on a project whose reindex can take minutes, before that
correct follow-up pass even starts.

Given that, full preemptive mid-file cancellation (and the partially-
rebuilt-`graph.db`-on-abort story it would need) was the wrong lift, exactly
as this ADR originally suspected. What was implemented instead:

- **`nexus_index::project`**: a process-wide `REINDEX_GENERATION` counter
  and a `CURRENT_REINDEX_ROOT` (the project path currently being rebuilt,
  set/cleared by the same `IndexingGuard` RAII that already manages
  `INDEXING_ACTIVE`). `note_possible_supersession(path)` bumps the
  generation only when `path` falls under the project currently in flight -
  a change for a *different* project isn't a supersession, it just queues
  normally behind `REINDEX_LOCK`, which was already fine.
- **`nexus_index::ingest::index_directory_checked`**: `index_directory`'s
  existing entry point now takes an optional `superseded: impl Fn() -> bool`
  checkpoint, polled every 25 files (`SUPERSESSION_CHECK_INTERVAL`) in the
  per-file walk inside `index_directory_inner` - cheap enough (one relaxed
  atomic load) to not matter, infrequent enough not to matter either way. A
  trip **rolls back** the in-progress transaction rather than committing a
  partial rebuild, so the previous, complete (if stale) `graph.db` survives
  untouched until the guaranteed follow-up pass replaces it correctly.
- **`nexusd::watcher`**: the debounce-thread closure - the one piece of this
  system that keeps running while the main loop is blocked in
  `index_project` - now calls `note_possible_supersession` for every real
  (non-noise) changed path, which is exactly what makes an in-flight pass
  able to notice a change on the same project without needing its own
  polling thread.
- **`status.get`**: `indexing.superseded_count` (alongside the existing
  `active`/`completed_count`), incremented once per bailed-out pass, in the
  same spirit as every other zero-in-the-common-case observability counter
  this ADR added.

This closes out the one item issue #58 still had open. See
[[Security-Model]]'s indexing-activity section for the field's shape.

## Context

[[ADRs/README|ADR 0011]] applied 4 concrete enforcement fixes verified out
of issue #58's original broad audit (bounded watcher channel, capped
traversal depth, file-size cap, query timeout) but deliberately left the
issue open, since #58 also asked for **observability** into these bounds -
seeing pressure against them, not just having them - plus a handful of
other audit items that needed their own verification pass.

This ADR records that closing pass: what observability was added, what was
confirmed to already be a non-issue, and what's being deliberately left out
of #58's scope with a stated reason, so the issue can close on an honest
accounting rather than staying open indefinitely on a vague "broader
audit".

## Decision

### Added: three observability fields in `status.get`

1. **`rss_kb`** - this process's resident set size
   (`crates/nexusd/src/control.rs::read_rss_kb`), read from
   `/proc/self/status`'s `VmRSS` line. No new dependency - `/proc` parsing
   is a few lines and matches this codebase's existing preference for
   direct Linux-specific reads over pulling in a cross-platform
   memory-info crate for one field. The whole `control` module is already
   `#[cfg(unix)]`-gated at its `mod control;` declaration in `main.rs` (see
   [[ADRs/README|ADR 0009]]), so there's no separate Windows/non-unix
   `cfg` needed here - `None` covers non-Linux Unix (macOS, where `/proc`
   doesn't exist).
2. **Watcher queue depth** - `WATCHER_QUEUE_DEPTH` and
   `WATCHER_CHANNEL_FULL_EVENTS` (`crates/nexusd/src/watcher.rs`),
   maintained by hand alongside every send/receive on the bounded channel
   from ADR 0011, since `SyncSender`/`Receiver` expose no `len()` of their
   own. Same pattern as the existing `WATCH_PRESSURE_EVENTS` counter:
   cheap, process-wide `AtomicUsize`s, zero in the common case, a loud
   nonzero signal when backpressure is real.
3. **Active indexing state** - `INDEXING_ACTIVE`/`INDEXING_COMPLETED_COUNT`
   (`crates/nexus-index/src/project.rs`), set/cleared by an RAII guard
   around the actual rebuild inside `index_project_locked` (so a panic
   mid-index can't leave `status.get` reporting `active: true` forever -
   the same poison-tolerant spirit as `REINDEX_LOCK` right above it).
   `index_project`/`index_project_deep` are the single choke point for
   every caller (MCP `index_repository`, the watcher's auto-reindex, and
   `projects.reindex`), and `REINDEX_LOCK` already serializes all of them
   process-wide - so this is honestly a 0/1 flag, not a real queue depth.

All three are plain fields added to `status.get`'s existing JSON shape -
no new metrics/telemetry framework, matching how `watch_budget` and
`pressure_events` were already exposed.

### Confirmed as already satisfied or not applicable - no code change

- **LSP process lifecycle** (`crates/nexus-index/src/lsp.rs`,
  `enrich.rs`) - already has `max_concurrent_servers` bounding,
  request/handshake timeouts, and graceful spawn/kill/degrade-cleanly
  behavior (see [[Security-Model]]'s LSP section). Verified unchanged.
- **Indexing concurrency bounding** - not applicable. Re-verified that
  `crates/nexus-index/src/ingest.rs::index_directory` is fully
  sequential (no `rayon`, no `thread::spawn`, no parallel iterator) -
  files are read and parsed one at a time in a single loop. "Bound
  indexing concurrency" presupposes concurrency that doesn't exist;
  adding an artificial concurrency limiter to inherently sequential code
  would be complexity with nothing to protect against. If indexing is
  ever parallelized in the future, a concurrency bound belongs in that
  same change, not speculatively ahead of it.
- **Watch budget observability** - already existed
  (`control.rs::status_get`'s `watch_budget` block, `WatchStatus` in
  `watcher.rs`) before this pass; this ADR's #2 above extends that same
  block with queue-depth fields rather than introducing a new one.

### Deliberately deferred, with reasons

- ~~**Background-work cancellation for a superseded reindex request.**~~
  Resolved by this ADR's own amendment above, dated the same day: traced
  through concretely (not by inspection) and confirmed to be the wasted-
  work case, not a data-loss case - a follow-up reindex reliably captures
  every change made during an in-flight one, since the watcher's debounce
  thread keeps queuing events on the bounded channel the whole time and the
  main loop drains and re-triggers on resumption. What was actually
  implemented is a lightweight cooperative checkpoint
  (`REINDEX_GENERATION`/`note_possible_supersession`/
  `index_directory_checked`), not full mid-file preemptive cancellation -
  see the amendment for the concrete trace and what was built.
- **A unified resource-governor abstraction** across all the bounds
  from ADR 0011 plus this pass's observability. Considered and rejected
  again for the same reason ADR 0011 gave: the enforcement points differ
  in kind (a channel bound, an argument clamp, a file-size check, a
  SQLite progress handler, a couple of atomics) with little shared logic
  to unify, and building one now would be speculative generality.

## Consequences

- An operator (via the GUI dashboard or a direct `status.get` call) can
  now see this process's actual memory footprint, whether the watcher's
  bounded channel has ever hit backpressure, and whether a reindex is
  running right now - the three gaps #58's own "Observability" section
  named as missing.
- Issue #58's checklist is now fully satisfied: the original observability
  gaps, plus (per this ADR's amendment) mid-flight reindex supersession,
  handled cooperatively rather than via full preemptive cancellation.

## Related

[[Security-Model]] · [[ADRs/README|ADR 0009]] · [[ADRs/README|ADR 0011]] ·
[issue #58](https://github.com/devNalyx/NexusContext/issues/58)
