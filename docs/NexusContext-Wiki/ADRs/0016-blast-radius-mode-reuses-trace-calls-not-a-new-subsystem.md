# 0016. Blast-radius mode for `detect_changes` reuses `trace_calls`/clamping, not a new subsystem

Status: Accepted
Date: 2026-08-27

## Context

Issue #89 asked for a richer `detect_changes`: instead of only "these
symbols were directly edited," an opt-in mode that also answers "what
depends on this, transitively, and how big is the blast radius." The issue
itself named the risk explicitly: don't build a new traversal or a new
test-coverage-mapping subsystem to get there - the graph can already answer
"who calls this" via `GraphStore::trace_calls` (the same BFS
`trace_call_path` exposes, with per-node `edge_kind` provenance since
[[0013-provenance-confidence-surfaced-through-trace-call-path]]), and the
depth/limit bounding this needs already exists as `clamp_depth`/
`clamp_limit` from #58's work.

## Decision

`detect_changes_blast_radius(repo_path, depth)` (`crates/nexus-index/src/
queries.rs`) is additive, not a rewrite: `detect_changes_direct`, a small
internal helper, factors out the existing git-diff-to-`NodeRecord` mapping
so both plain `detect_changes` and the new blast-radius path share it
byte-for-byte instead of diverging. Blast-radius mode then runs one
`GraphStore::trace_calls(name, Direction::Inbound, depth)` call per
directly-changed function - the exact same BFS `trace_call_path` already
runs, just started from every changed symbol instead of one caller-named
one - and unions/dedupes the results (excluding anything already in the
direct set) into a `BlastRadiusResult { direct, transitive, files_touched }`.
Each `transitive` entry is a `TracedNode`, carrying the same `edge_kind`
provenance `trace_call_path` already surfaces.

`crates/nexusd/src/tools.rs`'s `detect_changes` MCP handler adds
`blast_radius` (bool, default `false`), plus `depth`/`limit` - named and
clamped (`clamp_depth`/`clamp_limit`) identically to `trace_call_path`'s own
parameters, deliberately, so an agent that has used one immediately
understands the other. When `blast_radius` is `false` (the default), the
handler takes the exact same code path it always has - `index::detect_changes`
- and returns the exact same response shape; the blast-radius branch (BFS,
provenance mapping, summary counts) is not reached at all, so existing
callers see no behavior or cost change. When `true`, the response adds
`transitive`/`transitive_total`/`transitive_shown` (capped by `limit`, same
total-vs-shown honesty pattern as `trace_call_path`/`detect_dead_code`) and
a `summary` (`direct_count`, `transitive_count`, `files_touched`).

## Scope

Per the issue's explicit instruction: no test-coverage-mapping ("which
tests cover this changed code") was attempted - that's not answerable from
the current graph's structural signals without a new subsystem, so it's
left out entirely rather than approximated.

## Consequences

- No new BFS, no new bounding scheme, no new provenance model - a bug fix
  or improvement to `trace_calls`'s traversal or `edge_kind_provenance`'s
  tagging benefits `detect_changes`'s blast-radius mode automatically.
- `BlastRadiusResult`/`detect_changes_blast_radius` are additive exports
  from `nexus-index` (`crates/nexus-index/src/lib.rs`); no existing
  function signature changed.
- Cost is opt-in by construction: `blast_radius=true` runs one `trace_calls`
  walk per directly-changed function, so a diff touching many symbols is
  proportionally more expensive - same tradeoff `trace_call_path` already
  makes explicit via `depth`/`limit`, not a new unbounded-cost shape.

## Related

[[MCP-Tools]] ·
[[0013-provenance-confidence-surfaced-through-trace-call-path]] ·
[[0014-resource-observability-and-closing-out-58]]
