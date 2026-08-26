# 0011. Every repository-size/agent-request-dependent operation gets an explicit bound

Status: Accepted
Date: 2026-08-26

## Context

Issue #58 audited the daemon for operations whose cost scales with either
the size of the indexed repository or a value an MCP caller (an LLM agent,
not a human typing carefully) supplies directly, with no ceiling in the
code. Four concrete, verified gaps came out of that audit:

- The file watcher's debounced-event channel
  (`crates/nexusd/src/watcher.rs`) was `std::sync::mpsc::channel()` -
  unbounded. A receiver that falls behind (a slow reindex in progress, see
  `MIN_REINDEX_GAP`'s doc comment for how long that can run) let debounced
  batches queue up with no ceiling under a sustained flood of filesystem
  events.
- `trace_call_path` (`crates/nexusd/src/tools.rs`) and the GUI's
  call-graph visualization (`crates/nexusd/src/control.rs`) both accepted
  a caller-supplied `depth: u32` with no upper bound - only the *result
  count* (`limit`) was clamped via `clamp_limit`/`SERVER_MAX_LIMIT`.
  Traversal cost over the `CALLS` graph grows combinatorially with depth,
  not linearly with the result count, so a large `depth` from a bad or
  adversarial agent call could blow up latency and memory independent of
  `limit`.
- `crates/nexus-index/src/ingest.rs` read every file's full contents
  (`std::fs::read`) before parsing, with no size check first - a huge
  generated/vendored/minified file landing in an indexed tree loaded
  fully into memory regardless of whether it was ever going to produce a
  meaningfully useful structural index entry. Related to (though distinct
  from) the #17 OOM investigation, which fixed a different memory
  mechanism in the same pipeline.
- `run_cypher_query` (`crates/nexus-index/src/cypher.rs`) executed a
  caller-supplied query pattern against SQLite with no execution-time
  bound - a pathological or unselective query could run indefinitely,
  blocking whatever else needed that connection.

Each gap shares the same shape: cost driven by something the daemon
doesn't control (repository size, or what an agent decides to ask for),
with the corresponding bound simply never having been added.

## Decision

Adopt an explicit rule: **any operation whose cost scales with repository
size or a caller-supplied parameter must have a stated, enforced ceiling**
- not just the result-count caps ([[Security-Model]]'s
`SERVER_MAX_LIMIT`/`MAX_RETURNED_BYTES`/`MAX_RETURNED_LINES`) that already
existed, but every dimension cost can grow along: queue depth, traversal
depth, input size, and execution time.

Applied concretely:

1. **Bounded watcher channel** - `std::sync::mpsc::sync_channel(256)`
   (`WATCHER_CHANNEL_BOUND`) in place of the unbounded channel. A full
   channel makes notify's internal debounce thread block briefly on send
   rather than growing without limit; the receiving loop already drains
   the channel in one pass on every wake (see `run`'s drain-loop comment),
   so real-world depth rarely approaches the bound.
2. **Capped traversal depth** - `clamp_depth()` / `SERVER_MAX_DEPTH` (10),
   the same shape as the existing `clamp_limit()`/`SERVER_MAX_LIMIT`,
   applied everywhere `depth` is read from an MCP or control-API argument.
3. **File-size cap before indexing** - `MAX_INDEXABLE_FILE_BYTES` (5MB),
   checked via `stat` before `std::fs::read`, so an oversized file is
   never loaded into memory at all. Surfaced through the pipeline's
   existing per-file skip mechanism (`index_directory`'s "failed to index
   file, skipping" `Err` handling), not a new, separate skip path.
4. **Query execution timeout** - `run_cypher_query` now installs a
   `rusqlite` progress handler (`GraphStore::set_query_timeout`) before
   executing and clears it after, cooperatively interrupting the
   statement once a wall-clock budget (5s, `QUERY_TIMEOUT`) elapses.
   Chosen over hard-killing a thread: SQLite's progress-handler mechanism
   is designed for exactly this, and rusqlite surfaces the interruption as
   an ordinary `Err`, not a panic or an orphaned thread still holding the
   connection.

## Alternatives considered

- **Leave queue growth/traversal depth/file size/query time unbounded and
  rely on the daemon-level protections already in place** (systemd
  hardening, per-response size caps). Rejected: those protect against
  different failure modes (privilege escalation, oversized *responses*)
  but do nothing to stop an in-flight operation from consuming unbounded
  memory or CPU before it ever produces a response.
- **A single global "resource governor" abstraction covering all four**
  (a shared budget/timeout type used everywhere). Considered and deferred
  - the four gaps have different natural enforcement points (a channel
    bound, an argument clamp, a pre-read size check, a SQLite progress
  handler) with little real shared logic between them; building a unifying
  abstraction now would be speculative generality for problems that don't
  actually need to compose with each other.
- **Kill the query-execution thread on timeout instead of a cooperative
  progress handler.** Rejected: `rusqlite`'s `Connection` isn't `Sync` in
  a way that makes killing-from-outside safe/simple, and a hard kill risks
  leaving the SQLite connection or its WAL state in an inconsistent spot;
  the progress handler is the mechanism SQLite itself provides for exactly
  this case.

## Consequences

- A sustained filesystem event flood now produces bounded memory growth
  and a bounded stall on the watcher's internal thread, not unbounded
  queue growth.
- A `depth` value an agent supplies (deliberately or not) beyond 10 is
  silently clamped, matching how `limit` beyond `SERVER_MAX_LIMIT` already
  behaves - consistent, predictable degradation instead of a surprising
  failure mode.
- A repository containing one or more very large generated/vendored files
  no longer risks OOM-ing the indexer on account of those files alone;
  they're skipped and logged like any other per-file indexing failure,
  and the rest of the project still indexes normally.
- A pathological `run_cypher_query` call now fails with a clear "query
  timed out" error after a few seconds instead of hanging the connection
  (and whatever else is waiting on it) indefinitely.
- This ADR's four fixes are a scoped subset of issue #58, not its full
  original wishlist - see the issue and the PR that closed this slice of
  it for what remains open, if anything.

## Related

[[Security-Model]] · [[Architecture]] · [[Watcher-and-Freshness]] ·
[issue #58](https://github.com/devNalyx/NexusContext/issues/58)
