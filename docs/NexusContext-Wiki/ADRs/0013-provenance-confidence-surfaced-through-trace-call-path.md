# 0013. `CALLS` vs `CALLS_RESOLVED` provenance/confidence is surfaced through `trace_call_path`'s JSON response

Status: Accepted
Date: 2026-08-27

## Context

[[0008-lsp-resolved-edges-are-a-distinct-kind]] (issue #10) already stores
LSP-verified call edges as a distinct `CALLS_RESOLVED` kind, separate from
the static, name-based `CALLS` edges tree-sitter produces. `trace_calls`
and `dead_functions` already union both kinds when walking the call graph.

Issue #59 pointed out a gap in that otherwise-complete design: the
distinction lived in storage and traversal, but not in what an MCP caller
actually sees. `trace_call_path`'s tool *description* carried a prose
caveat ("resolution is name-based, not import-aware"), but its JSON
*response* returned a flat list of function-node records with no per-node
indication of which edge kind produced it. An agent calling the tool had
no way to tell a plausible-but-unverified name match apart from a hop
`rust-analyzer` actually confirmed - exactly the "NexusContext says A
calls B" vs. "NexusContext found a plausible name-based relationship"
conflation issue #59 describes.

## Decision

`GraphStore::trace_calls` now returns `Vec<TracedNode>` instead of
`Vec<NodeRecord>`, where `TracedNode { node, edge_kind }` tags each result
with the `EdgeKind` of the edge that reached it. Since the BFS's existing
`visited` set already ensures a node is only ever enqueued once, the edge
that produced a given BFS result *is* the edge that first reached it -
there is exactly one candidate kind to report, not several.

Multiple parents in different BFS layers, in principle, could reach the
same node via different edge kinds (e.g. static + resolved). This decision
reports the kind of whichever edge reached it first (i.e., the one that
actually produced the BFS result), not the set of every kind that happens
to link to it from any layer - this is simple, matches what the BFS
actually did to find the node, and doesn't turn every result into a
multi-valued provenance set that every consumer must then handle.

`trace_call_path` (`crates/nexusd/src/tools.rs`) maps each `TracedNode`'s
`EdgeKind` to explicit fields on the JSON node object:

```json
{
  "kind": "Function",
  "name": "...",
  "qualified_name": "...",
  "file": "...",
  "start_line": 1,
  "end_line": 2,
  "provenance": "tree-sitter",
  "resolution": "name-match",
  "confidence": "heuristic"
}
```

for a plain `CALLS` edge, or

```json
{
  "provenance": "lsp",
  "resolution": "semantic-symbol",
  "confidence": "exact",
  "...": "..."
}
```

for a `CALLS_RESOLVED` edge. The tool's description string was also
updated to state these fields explicitly rather than leaving them to be
discovered from the response shape alone.

## Scope

This only touches `trace_call_path`, the one MCP tool whose response
returns per-edge call-graph data without the caller already having chosen
the edge kind explicitly:

- `search_graph` returns name-matched nodes, not edges - out of scope.
- `detect_dead_code` reports node *absence* of any inbound `CALLS`/
  `CALLS_RESOLVED` edge, not a per-edge result - there's no single edge to
  tag a dead-code hit with, so it stays as-is.
- `query_graph` (the Cypher-lite DSL) already requires the caller to name
  the exact edge kind (`CALLS` or `CALLS_RESOLVED`) in the pattern itself -
  the distinction is already explicit in what the caller asked for, so
  there's nothing implicit left to surface.
- `call_graph_dot` (CLI-only Graphviz export) and the `nexus-cli trace`
  command were updated to keep compiling against the new
  `Vec<TracedNode>` return type; the CLI's plain-text `trace` output also
  now prints `[provenance, confidence]` per row for the same reason, though
  this is incidental to the MCP-facing change this ADR is about.

Explicitly out of scope, per issue #59 and the task that implemented this
ADR: a full re-architecture of the confidence model, a richer provenance
taxonomy (e.g. `import-resolution` as its own category, ambiguous/
medium/high confidence tiers beyond the two levels the current two edge
kinds actually produce), a schema migration beyond carrying the existing
`EdgeKind` through to the response, or LSP support for any language beyond
the existing Rust/rust-analyzer pilot (issue #10).

## Consequences

- Any future third edge kind on the call graph (e.g. a second language's
  LSP, or an import-resolution pass) must be added to
  `edge_kind_provenance` in `tools.rs` (and to
  `EdgeKind::from_edges_kind_str` in `graph.rs`) to get its own
  provenance/confidence pair - falling through to the conservative
  `tree-sitter`/`heuristic` default otherwise, which is safe (understates
  confidence) but not informative.
- `TracedNode` is a public, additive change to `nexus-index`'s API -
  `GraphStore::trace_calls` callers (`nexusd::tools`, `nexus-cli`,
  `nexus_index::queries::call_graph_dot`) all needed a small update to
  unwrap `.node` where they only wanted the `NodeRecord`, which is now
  done throughout the workspace.

## Related

[[MCP-Tools]] · [[0008-lsp-resolved-edges-are-a-distinct-kind]] ·
[[0004-name-based-call-resolution]] · [[Storage-and-Data-Model]]
