# MCP Tools

13 tools total, backed by the same SQLite graph — no separate index to keep
in sync, no silent fallback that hides what actually ran. Every tool caps
its response somehow; see [[Security-Model]] for why that became a
deliberate, tested property rather than an assumption.

## Index & read

| Tool | What it does |
|---|---|
| `index_repository` | Build or rebuild the knowledge graph for a directory. Run this before anything else on a project you haven't indexed yet. |
| `get_file_context` | Read a file, or a bounded line range, from an indexed project. No range + `full=false` (default) returns the first 300 lines with a truncation note. Every branch is now byte-capped, not just line-capped — see [[Security-Model]]. |
| `get_architecture` | Node/edge counts, busiest files by definition density, language breakdown. Cached, keyed on the project's `last_indexed_unix`, so repeated calls against an unchanged index skip SQLite entirely. |

## Search & trace

| Tool | What it does |
|---|---|
| `search_graph` | Structural search over indexed symbols by name substring — functions/types and markdown heading `Section`s. No embeddings required. |
| `search_code` | Grep-like full-text search over indexed file content via SQLite FTS5 — code and markdown alike, matched as a literal phrase. |
| `trace_call_path` | BFS over the `CALLS` graph to find callers/callees. Name-based resolution, not import-aware — see [[Known-Limitations]]. Response is capped; check `total_nodes` vs. `shown`. |

## Quality & change

| Tool | What it does |
|---|---|
| `detect_dead_code` | Functions with no inbound `CALLS` edge (excluding `main`). High false-positive rate is expected and stated in the tool's own description — treat hits as leads, not conclusions. |
| `detect_changes` | Maps uncommitted git changes to the graph symbols whose line range overlaps a diff hunk. |
| `query_planner` | Picks the cheapest retrieval strategy (file read / symbol search / semantic-or-keyword fallback) instead of the agent guessing. Returns which strategy it used. |

## Query & manage

| Tool | What it does |
|---|---|
| `query_graph` | Minimal ad-hoc Cypher-lite: exactly one pattern shape, `MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...] RETURN a|b`. `Kind` is `Function`/`Type`/`File`/`Section`. Fails clearly outside that shape rather than guessing. |
| `delete_project` | Removes a project's indexed data (graph + registry entry). Never touches the source directory. Destructive — gated behind the `full` preset. |
| `search_codebase` / `query_memory` | Real cosine-similarity semantic search over embedded chunks. Requires `embeddings.enabled = true` and a reachable endpoint — see [[Embeddings-and-Semantic-Search]]. `query_memory` is currently the same ranked search as `search_codebase`; richer RAG-style retrieval is a future enhancement, not built yet. |

## Why not all 13 are advertised by default

Every MCP session pays a fixed token cost just to load `tools/list`'s
schemas, regardless of whether that session ever calls most of them. A
`[tools]` config section controls what's actually advertised:

```toml
[tools]
preset = "standard"   # "minimal" (5) | "standard" (default, 9) | "full" (13)
# enabled = ["search_code", "get_architecture"]   # explicit list, overrides preset
```

- **`minimal`** (5): `index_repository`, `search_code`, `get_file_context`,
  `get_architecture`, `trace_call_path` — the read-heavy core loop.
- **`standard`** (9, the default): adds `search_graph`, `detect_changes`,
  `detect_dead_code`, `query_planner`.
- **`full`** (13): adds `delete_project` (destructive), `query_graph`
  (niche DSL), `search_codebase`/`query_memory` (embeddings-gated).

This was a deliberate fix, not the original design — see `README.md`
Phase 21/22 for the token-cost measurement that drove it.

## Related

[[Security-Model]] (response caps, `allowed_roots`) · [[Architecture]] ·
[[Configuration]] · [[Embeddings-and-Semantic-Search]]
