# Architecture

One Rust binary, `nexusd`, running as three different things depending on
how it's launched — never more than one purpose per process, and never
sharing a transport across purposes.

```
                    ┌──────────────────────────────┐
   MCP clients ──────► nexusd mcp  (stdio, per-session) │
 (Claude Code, etc.)  │   listTools / callTool        │
                    │   search_graph · trace_call_path│
                    │   get_architecture · query_planner│
                    └──────────────────────────────┘
                               │
                               ▼  shared SQLite graph.db (WAL mode)
                               ▲
                    ┌──────────────────────────────┐
    GUI + Shell ext ──► nexusd serve (Unix socket, systemd)│
                    │   status · projects · config    │
                    │   search.adhoc · reindex         │
                    └──────────────────────────────┘
```

## The three ways `nexusd` runs

- **`nexusd mcp`** — an MCP stdio server. Newline-delimited JSON-RPC 2.0 on
  stdin/stdout, spawned per-session by whatever agent connects (Claude Code,
  etc.). Logs go to **stderr only** — stdout is reserved for the protocol
  stream, and corrupting it with a stray log line breaks the client's
  parser. See [[MCP-Tools]] for what it exposes.
- **`nexusd serve`** — the long-lived background daemon, normally run as a
  `systemd --user` service. Owns the [[Watcher-and-Freshness|background
  file watcher]] and hosts the **control API**: a Unix domain socket at
  `$XDG_RUNTIME_DIR/nexuscontext/nexuscontext.sock`, same JSON-RPC framing
  as the MCP transport but a distinct method namespace (`status.*`,
  `config.*`, `projects.*`, `search.adhoc`, `viz.call_graph`, `stats.get`).
  This is what the GTK4 Manager app and the GNOME Shell extension talk to —
  never MCP, never stdio.
- **`nexus` (the CLI)** — a separate binary for humans: manual
  reindex/search/trace/etc. Goes through the exact same shared code path as
  the MCP tools and the control API (`nexus_index::touch_and_catchup` and
  friends) — see [[Watcher-and-Freshness]] for why that consolidation
  mattered.

Two transports, deliberately kept apart: an MCP client attached to stdio
never competes with a GUI session on the control socket, and vice versa.

## Component breakdown

- **Ingestion engine** — tree-sitter parsing per language via the
  `tree-sitter-tags` mechanism (see [[Indexing-Pipeline]]), a two-pass
  cross-file call resolver, and a markdown heading parser for docs.
- **Knowledge graph** — SQLite (WAL mode) at
  `~/.local/share/nexuscontext/<project-hash>/graph.db`. Nodes
  (`File`/`Function`/`Type`/`Section`) and edges (`Defines`/`Calls`/
  `Contains`), full-text search via FTS5, a minimal Cypher-lite query
  layer. No dedicated vector database — see [[Storage-and-Data-Model]].
- **Embeddings pipeline** *(optional)* — an OpenAI-compatible
  `/v1/embeddings` HTTP client. Off by default; see
  [[Embeddings-and-Semantic-Search]].
- **Control API** — the `serve`-mode Unix socket described above.
- **Desktop GUI** — "NexusContext Manager", GTK4 + libadwaita. See
  [[GUI-and-Extension]].
- **GNOME Shell extension** — a thin top-bar status indicator, GJS. Also in
  [[GUI-and-Extension]].

## Design principle: the agent is the intelligence, NexusContext is the memory

No LLM lives in the daemon. It builds structure and answers queries — the
calling agent still does all the reasoning. This is why most tools need no
embeddings at all: the agent reads structural/text results and reasons over
them itself, the same way it would reason over a `grep` result, just with
far less noise.

## Related

[[MCP-Tools]] · [[Indexing-Pipeline]] · [[Storage-and-Data-Model]] ·
[[Watcher-and-Freshness]] · [[GUI-and-Extension]] · [[Configuration]]
