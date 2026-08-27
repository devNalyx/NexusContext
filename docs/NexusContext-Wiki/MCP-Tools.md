# MCP Tools

12 tools total, backed by the same SQLite graph — no separate index to keep
in sync, no silent fallback that hides what actually ran. Every tool caps
its response somehow; see [[Security-Model]] for why that became a
deliberate, tested property rather than an assumption.

## Index & read

| Tool | What it does |
|---|---|
| `index_repository` | Build or rebuild the knowledge graph for a directory. Run this before anything else on a project you haven't indexed yet. `deep: true` also runs LSP-resolved-symbol enrichment (Rust only, opt-in via `[lsp]`) — see [[Storage-and-Data-Model]]. |
| `get_file_context` | Read a file, or a bounded line range, from an indexed project. No range + `full=false` (default) returns the first 300 lines with a truncation note. Every branch is now byte-capped, not just line-capped — see [[Security-Model]]. |
| `get_architecture` | Node/edge counts, busiest files by definition density, language breakdown, plus an `index_freshness` field (last-indexed time, whether the project is currently "warm"). Cached, keyed on the project's `last_indexed_unix`, so repeated calls against an unchanged index skip SQLite entirely — freshness is computed fresh on every call regardless, so it never reads stale from the cache. Default response/cost is unchanged. Optional `grouped=true` (issue #90) adds a directory-based structural breakdown built from every node's existing `file_path` — no new indexing. `depth` (default `1`) controls how many path components form each group's key (e.g. depth `1` groups by top-level directory, depth `2` by the first two components). Response then gains a `grouped` object: `groups` (per-directory `path`, `total_nodes`, and `node_counts` broken down by kind — function/type/etc.), `within_group_edges` (edges whose endpoints share a group), and `cross_group_edges` (edges crossing between two different groups, tallied per `from`/`to` pair — the "which directories actually call into which" signal). This is deliberately structural grouping by path only, not semantic subsystem/layer inference — see ADR 0017. |

## Search & trace

| Tool | What it does |
|---|---|
| `search_graph` | Structural search over indexed symbols by name substring — functions/types and markdown heading `Section`s. |
| `search_code` | Grep-like full-text search over indexed file content via SQLite FTS5 — code and markdown alike, matched as a literal phrase. |
| `trace_call_path` | BFS over the `CALLS` graph (unioned with `CALLS_RESOLVED` when a `deep` reindex has run) to find callers/callees. Name-based resolution, not import-aware — see [[Known-Limitations]]. Response is capped; check `total_nodes` vs. `shown`. `depth` is capped independently of `limit`, see [[Security-Model]]. **Each returned node also carries `provenance`/`resolution`/`confidence`** (issue #59), tagged from whichever edge first reached that node in the BFS: a plain `CALLS` hop reports `{"provenance": "tree-sitter", "resolution": "name-match", "confidence": "heuristic"}`; a `CALLS_RESOLVED` hop (only present after a `deep` reindex with LSP enrichment — see #10) reports `{"provenance": "lsp", "resolution": "semantic-symbol", "confidence": "exact"}`. Treat `heuristic` hops as plausible-but-unverified and `exact` hops as backed by rust-analyzer's semantic symbol resolution. |

## Quality & change

| Tool | What it does |
|---|---|
| `detect_dead_code` | Functions with no inbound `CALLS`/`CALLS_RESOLVED` edge (excluding `main`). High false-positive rate is expected and stated in the tool's own description — treat hits as leads, not conclusions. Optional `path_prefix` (e.g. `"pkg/events"`) scopes results to a subdirectory or exact file — a real path-prefix match, not a naive string prefix, so it won't false-match a sibling directory like `pkg/events-vendor`. Fixes issue #77: on a monorepo, an unscoped call was dominated by vendored/generated code. |
| `detect_changes` | Maps uncommitted git changes to the graph symbols whose line range overlaps a diff hunk. |
| `query_planner` | Picks the cheapest retrieval strategy (file read / symbol search / keyword fallback) instead of the agent guessing, and returns the actual answer in-band alongside which strategy it used — no second call needed. Also carries `index_freshness`. |

## Observability

| Tool | What it does |
|---|---|
| `get_session_usage` | Per-tool call/error/output-byte counters (plus a rough bytes/4 token estimate) for *this* MCP session only — in-memory, resets when the `nexusd mcp` process does. Also reports `schema_tax` (the fixed per-session cost of every tool's own schema) and `reads_avoided` (an auditable, conservative counterfactual — successful calls to an explicit tool allow-list that plausibly substituted a manual read/grep). Not the lifetime totals `stats.get`/the GUI Usage tab show — see [[Storage-and-Data-Model]] for those. |

## Query & manage

| Tool | What it does |
|---|---|
| `query_graph` | Minimal ad-hoc Cypher-lite: exactly one pattern shape, `MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...] RETURN a|b`. `Kind` is `Function`/`Type`/`File`/`Section`. Fails clearly outside that shape rather than guessing. |
| `delete_project` | Removes a project's indexed data (graph + registry entry). Never touches the source directory. Destructive — gated behind the `full` preset. |

## Why not all 12 are advertised by default

Every MCP session pays a fixed token cost just to load `tools/list`'s
schemas, regardless of whether that session ever calls most of them. A
`[tools]` config section controls what's actually advertised:

```toml
[tools]
preset = "standard"   # "minimal" (5) | "standard" (default, 10) | "full" (12)
# enabled = ["search_code", "get_architecture"]   # explicit list, overrides preset
```

- **`minimal`** (5): `index_repository`, `search_code`, `get_file_context`,
  `get_architecture`, `trace_call_path` — the read-heavy core loop.
- **`standard`** (10, the default): adds `search_graph`, `detect_changes`,
  `detect_dead_code`, `query_planner`, `get_session_usage`.
- **`full`** (12): adds `delete_project` (destructive) and `query_graph`
  (niche DSL).

This was a deliberate fix, not the original design — see `README.md`
Phase 21/22 for the token-cost measurement that drove it, Phase 29
for `get_session_usage` joining `standard`, and Phase 31 for its
`schema_tax`/`reads_avoided` fields.

Every tool here works identically on Linux, macOS, and Windows — none of
them depend on `nexusd serve` (the control API, background watcher, GUI
target), which is the one piece not yet ported to Windows. See
[[Known-Limitations]].

## Related

[[Security-Model]] (response caps, `allowed_roots`) · [[Architecture]] ·
[[Configuration]]
