# 0017. `get_architecture`'s grouped mode is directory/path-based, never semantic subsystem inference

Status: Accepted
Date: 2026-08-27

## Context

Issue #90 asked for `get_architecture` to answer subsystem-shaped questions
("explain the authentication subsystem") instead of only a flat node/edge
summary - today an agent asking that has to fall back to `search_graph`/
`search_code` on likely-relevant names and rebuild the picture itself. The
issue's proposed direction was explicit about scope: investigate what's
cheaply derivable from the existing graph without new indexing
infrastructure, and do not attempt automatic subsystem/layer detection
(e.g. inferring "this is the API layer") - that's a much bigger, fuzzier
problem than this issue should take on.

Every graph node already carries `file_path` (`nodes` table,
`crates/nexus-index/src/graph.rs`), so directory structure is fully
derivable with no new extraction work - the same "reuse what's already
there" shape issue #89's blast-radius mode took for `detect_changes` (see
[[0016-blast-radius-mode-reuses-trace-calls-not-a-new-subsystem]] once
merged).

## Decision

`GraphStore::directory_groups(depth)` groups every node by the first
`depth` path components of its `file_path`'s parent directory (`.` for a
file with no directory component), and classifies every edge as
within-group or cross-group by looking up its two endpoints' groups - a
single pass over `nodes` and `edges`, no new tables, no new indexing pass.
`get_architecture_grouped(repo_path, depth)` (`crates/nexus-index/src/
queries.rs`) wraps this alongside the existing flat `ArchitectureSummary`,
sharing one `GraphStore::open` call via a factored-out `architecture_summary`
helper so both the plain and grouped paths compute the flat half
identically.

`get_architecture`'s MCP tool gains `grouped` (bool, default `false`) and
`depth` (integer, default `1`, clamped via the existing `clamp_depth`).
When `grouped` is false/omitted, the handler takes the exact same code
path and cache key prefix it always has - `directory_groups` is never
reached, so existing callers see no behavior or cost change. When `true`,
the response additionally carries a `grouped` object: per-directory
`groups` (`path`, `total_nodes`, `node_counts` by kind), `within_group_edges`,
and `cross_group_edges` (`from`/`to`/`count`, sorted busiest-pair-first) -
the "which directories actually depend on which" signal the issue asked
for.

Grouping is deliberately structural (path-based) only. There is no
attempt to name, cluster, or infer what a directory *is* ("this is the API
layer") - `path` is always the literal directory string, never a guessed
label. A caller that wants semantic subsystem framing applies that
judgment itself on top of this data; the tool doesn't guess it for them.

## Alternatives considered

- **Semantic subsystem/layer inference** (clustering by naming
  conventions, import patterns, or an LLM-assisted label). Rejected per
  the issue's explicit instruction - this is a much fuzzier problem
  (what counts as a "layer" varies per project and per language) that
  would need its own accuracy story, its own false-positive caveats (like
  `detect_dead_code`'s), and likely a new extraction pass. Directory
  structure already carries most of the same signal for free.
- **A separate new tool** (e.g. `get_architecture_grouped`) instead of an
  opt-in parameter on the existing one. Rejected to match the pattern
  #89 established for `detect_changes`/`blast_radius`: one tool, one opt-in
  parameter, so the flat and grouped views stay trivially comparable and
  a caller doesn't need to learn a second tool's schema for a strict
  superset of the same data.
- **A fixed, non-configurable grouping depth.** Rejected: a monorepo's
  meaningful boundary might be one level deep (`services/`) or several
  (`services/api/handlers/`) - `depth` lets a caller pick, defaulting to
  `1` (top-level directories) as the cheapest, most broadly useful default.

## Consequences

- No new indexing/extraction machinery - a project already indexed today
  can use `grouped=true` immediately after upgrading, no reindex required.
- `directory_groups` is O(nodes + edges) per call (two full scans, no
  per-row query) - cheap relative to the graph sizes this tool already
  targets, and cached the same way the flat summary is (keyed on `grouped`/
  `depth` too, so a `grouped=true` call never serves a stale flat-only
  cache entry or vice versa).
- `cross_group_edges` is a raw directed tally (`from` -> `to`), not a
  symmetric "these two directories relate" flag - a caller that wants an
  undirected view sums both directions itself.
- Explicitly does not answer "what subsystem is this" - only "which
  directories exist and how do they call into each other." Closing that
  gap, if ever wanted, is future work with its own accuracy tradeoffs, not
  something this ADR's grouping quietly grew into.

## Related

[[MCP-Tools]] ·
[[0005-mcp-tool-presets]] ·
[[0016-blast-radius-mode-reuses-trace-calls-not-a-new-subsystem]]
