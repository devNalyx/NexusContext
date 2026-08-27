# 0014. Resource observability closes out issue #58, with two items explicitly deferred

Status: Accepted
Date: 2026-08-27

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

- **Background-work cancellation for a superseded reindex request.**
  Partially, not fully, handled today: `run`'s drain loop in
  `watcher.rs` already *coalesces* every debounced burst that arrives
  while one reindex is in flight into a single follow-up pass (see its
  own doc comment - this was itself a fix for a real repeated-reindex
  incident), so redundant re-triggering from the same burst of file
  events is already solved. What's genuinely still missing: once a
  reindex has actually *started* (past the coalescing point,
  `nexus_index::index_project` is running), there's no way to cancel it
  mid-flight if a newer request supersedes it - the running rebuild
  always runs to completion, however long that takes on a large
  repository, before anything else touching that project can proceed
  (`REINDEX_LOCK` blocks other callers, not just other reindexes of the
  same project). This is a real gap, not a documented non-issue, but a
  nontrivial one: mid-rebuild cancellation would need either a
  cooperative cancellation check threaded through `index_directory`'s
  per-file loop (similar in spirit to `run_cypher_query`'s progress
  handler from ADR 0011, but through hand-written code instead of a
  SQLite hook) or accepting a partially-rebuilt, inconsistent
  `graph.db` on abort - real design work, not a small addition. Left as
  a follow-up issue rather than folded into this pass.
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
- Issue #58's checklist is now honestly satisfied except for mid-flight
  reindex cancellation, tracked as its own follow-up rather than left
  vague inside #58.

## Related

[[Security-Model]] · [[ADRs/README|ADR 0009]] · [[ADRs/README|ADR 0011]] ·
[issue #58](https://github.com/devNalyx/NexusContext/issues/58)
