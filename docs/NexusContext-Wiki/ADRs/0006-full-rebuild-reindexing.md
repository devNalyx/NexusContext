# 0006. Reindexing is a full rebuild, not incremental diffing

Status: Accepted
Date: Original design, reaffirmed through Phase 17-20 watcher work

## Context

The alternative to a full rebuild is per-file incremental graph diffing:
on a file change, retract only the edges/nodes that file contributed
(including cross-file edges pointing *into* the changed file, e.g. a
`CALLS` edge into a renamed function) and re-insert the new ones. That's
real complexity - correctly diffing a graph, not just a table - for a
workload where a full rebuild of this project's own graph already
completes in low single-digit seconds even after the Phase 28 OOM fix.

## Decision

Every reindex is `GraphStore::clear()` followed by a full re-walk of the
project. The one deliberate optimization on top: embeddings reuse (keyed
by `qualified_name`, stable across a rebuild unlike `node_id`) so a
routine catch-up reindex on an embeddings-enabled project doesn't re-pay
the embeddings API cost for unchanged chunks.

## Alternatives considered

- **Full per-file incremental diffing.** Considered and explicitly
  deferred (not rejected outright) - `README.md`'s Open Risks section
  flags this as "worth a full-reindex fallback if incremental graph
  diffing gets too complex early on," i.e., the option to revisit this is
  intentionally left open, not closed.
- **Incremental re-walk without full graph diffing** (e.g., only re-parse
  changed files but never retract stale cross-file edges). Rejected as
  worse than either extreme: it would accumulate silently-stale edges
  over time with no full-rebuild safety net to catch them.

## Consequences

- Correctness is simple to reason about: after any reindex, the graph
  exactly reflects the current file tree, full stop - no risk of a stale
  edge surviving a diff bug.
- Reindex cost scales with project size on every trigger, not just on the
  files that actually changed - the watcher's own gating
  ([[Watcher-and-Freshness]], Phases 17-20) exists specifically to avoid
  triggering this full-rebuild cost more often than necessary, rather than
  making the rebuild itself cheaper.
- The Phase 28 OOM fix ([[Indexing-Pipeline]]) was a direct consequence of
  this design: a full re-walk means *every* file, including a single
  pathological minified one, gets fully re-processed on every rebuild -
  an incremental scheme touching only changed files would have contained
  the blast radius differently (though not eliminated the underlying
  per-file memory bug).

## Related

[[Indexing-Pipeline]] · [[Watcher-and-Freshness]] · [[Known-Limitations]]
