# NexusContext

**Objective:** A self-hosted, lightweight binary daemon that provides a standardized MCP interface for local codebase indexing and structural code intelligence (knowledge graph, optionally enriched with LSP-resolved symbols) — with agent-harness integrations (OpenCode) as the primary interactive surface. The legacy Linux/GNOME desktop GUI is maintenance-only and deprecated.

> **Note on the sections below:** Sections 1-3 and 5-6 describe the *current* system and are kept up to date. The old phase-by-phase build log is preserved as a frozen historical record in [`docs/Phase-History.md`](docs/Phase-History.md) - it intentionally keeps historical claims (old tool counts, removed subsystems) as they were at the time. For current behavior, use the wiki and the current README sections, not the archive.
>
> **Working in this repo with coding agents?** Start with [`AGENTS.md`](AGENTS.md) for build/test/conventions, [`CONTRIBUTING.md`](CONTRIBUTING.md) for PR/release workflow, and [`docs/NexusContext-Wiki/Agentic-Development.md`](docs/NexusContext-Wiki/Agentic-Development.md) for the NexusContext-specific agent workflow.

---

## 1. Architecture Overview

```
                     ┌─────────────────────────────┐
                     │        nexusd (daemon)       │
                     │  Rust binary, always-on      │
                     │                              │
   MCP clients ──────┤  MCP Server (JSON-RPC/stdio) │
   (IDE, CLI agents) │                              │
                     │  Ingestion Engine             │
   File watcher ─────┤   - tree-sitter parsing       │
   (bounded queue)   │   - resource-bounded          │
                     │                              │
                     │  Knowledge Graph (SQLite)      │
                     │   - nodes/edges, Cypher-lite   │
                     │   - FTS5 full-text search      │
                     │                              │
   rust-analyzer ────┤  Optional LSP Enrichment       │
   (opt-in, `--deep`)│   - Rust only today            │
                     │                              │
   GUI / extension ──┤  Control API (Unix socket)    │
                     └─────────────────────────────┘
```

Two transports, two purposes:
- **Stdio JSON-RPC** — reserved for MCP clients (IDE extensions, CLI agents). This is the actual MCP spec transport and shouldn't be shared with anything else.
- **Local Unix domain socket** — a separate control/status API for the GUI and GNOME extension (indexing progress, watched paths, config, ad-hoc search). Keeps the GUI decoupled from whatever MCP client happens to be attached to stdio at the time.

The daemon runs as a `systemd --user` service on Linux (Windows ships the MCP server + CLI tier only, no GUI/systemd unit), independent of any GUI. The GUI is a client, not a requirement — the tool must be fully usable headless.

## 2. Component Breakdown

**Ingestion Engine**
- Directory watcher (`notify` crate), git-diff-aware: on file change, re-parse only the changed files rather than polling everything. The watcher's internal channel is bounded (`WATCHER_CHANNEL_BOUND`) so a flood of filesystem events can't grow unbounded memory.
- Tree-sitter parsers per language, extracting functions/classes/interfaces as node boundaries instead of naive line-splitting.
- **Layered ignore rules**: hardcoded patterns (`.git`, `node_modules`, build dirs) → `.gitignore` hierarchy → project-specific `.nexusignore` (gitignore syntax) for one-off excludes. Symlinks always skipped, with `O_NOFOLLOW` defense-in-depth against symlink-substitution TOCTOU (see ADR 0015 - this closes the easy case, not full TOCTOU-proofing).
- Resource bounds added this cycle (issue #58): a per-file indexable-size cap (`MAX_INDEXABLE_FILE_BYTES`, 5 MiB - larger files are skipped, not truncated-and-indexed), a traversal-depth cap, and a graph-query timeout (`QUERY_TIMEOUT`, 5s, enforced cooperatively via SQLite's progress handler) so a pathological repo or query can't hang or OOM the daemon.
- Incremental re-indexing — only re-parse changed files, not the whole project, on save; cooperative reindex cancellation if a project is superseded by a newer reindex request mid-run.

**Knowledge Graph Layer** *(the main structural addition over the original proposal)*
- Every ingested file becomes graph nodes (`File`, `Function`, `Type`, `Section` for markdown headings, ...) linked by edges (`CALLS`, `CALLS_RESOLVED`, `CONTAINS`, ...) derived from the tree-sitter AST, plus optional LSP-verified edges.
- Stored in SQLite (not a dedicated graph DB) at `~/.local/share/nexuscontext/<project-hash>/graph.db` — cheap, embeddable, and a natural fit for graph traversal queries via recursive CTEs or a small Cypher-lite query layer (`query_graph`).
- This is what makes `trace_call_path`, `get_architecture`, `detect_changes` (git-diff → affected-symbols mapping), and dead-code detection possible. There is no semantic/embedding layer of any kind - retrieval is entirely structural (graph) and lexical (FTS5), see ADR 0010.
- Call resolution is name-based, not import-aware: same-file matches win, a cross-file call resolves only if the callee name is unique project-wide, and 2+ ambiguous candidates leave no edge at all (which means an ambiguous callee can silently read as dead code to `detect_dead_code`). Each `CALLS` edge carries `provenance`/`resolution`/`confidence` metadata so a caller can tell a plain name-match apart from an LSP-verified one (see below).

**Optional LSP-Resolved-Symbol Enrichment** *(off by default; Rust only today via `rust-analyzer`)*
- Never load-bearing: a missing/failing LSP server always degrades to the static tree-sitter-only index rather than failing the reindex, and it only ever runs on an explicit `deep` request (`index_repository`'s `deep: true` argument, or `nexus reindex --deep`) - the ordinary watcher-driven auto-reindex never pays this cost.
- When it runs, semantically-verified call edges are recorded as `CALLS_RESOLVED` with `provenance: "lsp"`, `resolution: "semantic-symbol"`, `confidence: "exact"` (vs. `provenance: "tree-sitter"`, `resolution: "name-match"`, `confidence: "heuristic"` for a plain name-based edge) — exposed to callers via `trace_call_path`.
- `[lsp]` config: `enabled` (bool, default false), `server_command` (default `rust-analyzer`), `max_concurrent_servers` (default 2 - the daemon evicts the least-recently-used server instance past this cap), `request_timeout_secs` (default 10).
- A single-language pilot deliberately, not a multi-language matrix - see issue #10 and ADR 0008.

**MCP Server**
- `listTools` / `callTool` per spec, newline-delimited JSON-RPC 2.0 over stdio. Logging goes to stderr exclusively - stdout is reserved for the protocol stream.
- 12 tools total, backed by the same SQLite graph: `index_repository`, `search_graph`, `trace_call_path`, `get_file_context`, `get_architecture`, `detect_changes`, `delete_project`, `detect_dead_code`, `search_code`, `query_planner`, `query_graph`, `get_session_usage`. See [`docs/NexusContext-Wiki/MCP-Tools.md`](docs/NexusContext-Wiki/MCP-Tools.md) for full schemas/limits per tool - not duplicated here to avoid this file drifting out of sync with the real schemas again.
- By default only 10 of these 12 are advertised to an agent (the `standard` preset) to cut the fixed per-session token cost of loading tool schemas; `full` adds the destructive `delete_project` and the niche `query_graph` ad-hoc DSL. See the `[tools]` config section.
- `allowed_roots` (opt-in, empty/unrestricted by default) is enforced consistently across every `repo_path`-accepting tool, not just `index_repository` - closed as issue #61.
- `get_session_usage`: per-tool call/error/output-byte counters (plus a rough bytes/4 token estimate) for the calling agent's own MCP session only - resets when the `nexusd mcp` stdio process does, not a lifetime total. Also reports the fixed per-session `schema_tax` (bytes of every tool's own schema, paid once regardless of usage) and a `reads_avoided` counterfactual (successful calls to an explicit allow-list of tools that plausibly substituted a manual file read/grep, plus the bytes/estimated-tokens they returned).
- `query_planner`/`get_architecture` responses carry an `index_freshness` field: whether the project is registered, when it was last indexed, and whether the background watcher currently considers it "warm" (actively kept in sync) or cold (may be behind the working tree) - a cheap, in-band staleness signal instead of a separate call.
- **Tool-description caveats** (kept in the schema text or the wiki, deliberately not repeated at length here):
  - `trace_call_path`/`detect_dead_code`: name-based resolution caveats above; call-graph quality varies by language (solid for Rust/Python/JS/TS/Go/Java/Ruby; structural-only, no call edges, for C/C++/C#/PHP - see `language.rs`).
  - `query_planner`: a specific file goes straight to `get_file_context`, a single identifier-like token goes to `search_graph`, and a descriptive multi-word query gets a keyword fallback over the graph - there is no semantic-search arm.
  - `query_graph`: not full Cypher - exactly one pattern shape, `MATCH (a:Kind)-[:EDGE_KIND]->(b:Kind) [WHERE a.name = 'value' or b.name = 'value'] RETURN a|b`. `Kind` is `Function`, `Type`, `File`, or `Section`. Fails with a clear error for anything outside that shape rather than guessing.

**Control API (for OpenCode panels / legacy GUI, not MCP)**
- Unix socket, same JSON-RPC framing for consistency, but a distinct method namespace (`status.*`, `config.*`, `search.adhoc`).
- Exposes: indexing status/progress, per-project stats, config get/set, manual reindex trigger, ad-hoc search for the panels' search box. The OpenCode plug wraps the read-only subset in a typed RPC contract; no view opens this socket directly.

**Desktop GUI — "NexusContext Manager" (deprecated; maintenance-only)**
- GTK4 + `libadwaita` via `gtk-rs`, native Ubuntu/GNOME look, Linux/GNOME-only. CI and release builds exclude it on macOS/Windows.
- **Deprecation notice:** new interactive work belongs in the OpenCode plug (`integrations/opencode-nexuscontext/`), which exposes the same daemon data through a typed RPC contract, a cross-platform TUI session panel, and an experimental Desktop panel. See [#107](https://github.com/devNalyx/NexusContext/issues/107). The GTK app will be removed after that panel reaches parity; it receives no new features.
- Views in current source: Dashboard (status + inotify watch budget), Projects (index/reindex/import/export/delete), Search, Architecture, Visualize (Graphviz PNG), Review (read-only), Usage (lifetime aggregates), Logs (last 300 lines). There is no Config view in current source, despite older docs.
- Talks to the daemon exclusively over the control socket. Never touches stdio.
- Not required to be running for the daemon or MCP integrations to work — it's a management/inspection tool.

**Third-Party CLI Integrations** *(`integrations/`, outside the daemon's own release cycle)*
- `integrations/opencode-nexuscontext-statusline/` (Phase 31, #11): an opencode plugin that mirrors `get_session_usage`'s "reads avoided" tally to disk as an agentic session progresses. Not a rendered status line - opencode has no plugin hook for its own status bar yet (linked issues in that directory's README) - so this ships the live data feed and documents wiring it into an external status line (tmux, `ocstatusline`) as the honest stopgap.

**GNOME Shell Extension (optional, thin)**
- Deliberately minimal: a top-bar indicator only.
  - Icon changes state (idle / indexing / error).
  - Dropdown: quick stats + a "Search…" entry that either does an inline quick lookup or launches the full GTK4 app.
- Runs inside `gnome-shell`'s process (GJS) — this is why it must stay thin. Anything heavier belongs in the GTK4 app, not the extension: Shell extensions that do real work are a common source of Shell crashes and are the most likely part of this stack to break across GNOME version upgrades.

## 3. Technical Stack

| Concern | Choice |
|---|---|
| Daemon language | Rust |
| Knowledge graph | SQLite, WAL mode (nodes/edges, FTS5 for content search, Cypher-lite traversal via `query_graph`) |
| Vector/semantic engine | **None.** The original proposal's embeddings/semantic-search subsystem was built (Phase 12), then removed entirely (issue #62, ADR 0010) once the structural graph alone proved sufficient for every shipped tool. Retrieval today is purely structural (graph) and lexical (FTS5) |
| Parsing | tree-sitter via community `TAGS_QUERY` conventions (11 languages) plus a hand-rolled markdown heading parser for `.md`/`.markdown` docs |
| Optional LSP enrichment | `rust-analyzer` only today, opt-in via `[lsp] enabled = true` + `--deep`; never runs on the ordinary auto-reindex path (see ADR 0008) |
| MCP transport | JSON-RPC 2.0 over stdio |
| GUI/control transport | JSON-RPC 2.0 over Unix domain socket |
| GUI toolkit | GTK4 + libadwaita (`gtk-rs`), Linux only |
| Shell integration | GNOME Shell extension (GJS), status-only |
| Auto-sync | `notify-debouncer-mini`, 2s debounce, `serve`-mode only, bounded watcher channel |
| Config | TOML, `~/.config/nexuscontext/config.toml` + env var overrides (`NEXUS_CONFIG_DIR`, `NEXUS_CACHE_DIR`, `NEXUS_LOG_LEVEL`, `NEXUS_LOG_FORMAT=json`) |
| Data dir | `~/.local/share/nexuscontext/` (or `NEXUS_CACHE_DIR` if set) |
| Service management | `systemd --user` unit, autostart (Linux); MCP server + CLI only on Windows, no service manager integration |
| Logging | `tracing` crate; `NEXUS_LOG_FORMAT=json` for structured/machine-parseable logs, plain text by default |
| Platforms | Linux (x86_64, arm64), Windows (x86_64, arm64) — MCP+CLI tier, macOS (Apple Silicon) — MCP+CLI tier |

## 4. Phase History (archived)

The detailed phase-by-phase build log (33 phases, bugs found/fixed, dogfooding notes) is preserved as a frozen historical record in [`docs/Phase-History.md`](docs/Phase-History.md).

It is intentionally **not** current-state documentation. For current behavior, use:

- [README sections 1-3 and 5-6](#1-architecture-overview) for the product overview
- [`docs/NexusContext-Wiki/`](docs/NexusContext-Wiki/Home.md) for current-state topic docs
- [`docs/NexusContext-Wiki/ADRs/README.md`](docs/NexusContext-Wiki/ADRs/README.md) for durable decisions

## 5. Why This Counts as "Full-Fledged"

A daemon alone is a backend, not a tool. What makes this complete for a Linux desktop user:
- Headless-first: daemon + MCP server work with zero GUI, so IDE/agent integration isn't blocked on the GUI being built.
- A real inspection/management surface (GTK4 app) for the parts a CLI is bad at — browsing search results, seeing indexing status at a glance.
- Desktop-native integration (top-bar status) without over-investing in Shell extension surface area, which is the most fragile part of any GNOME-integrated tool.
- Proper packaging (.deb/Flatpak + systemd unit) so it installs and runs like a normal Ubuntu service, not a script someone has to remember to start.

## 6. Open Risks / Decisions to Revisit

- ~~**LanceDB Rust binding maturity**~~ — moot: the vector-store pick was never revisited once the knowledge graph proved sufficient for every structural tool built so far (full-text search also landed on SQLite FTS5 rather than a separate vector engine). Revisit only once the embeddings pipeline actually gets built.
- ~~**Tree-sitter grammar coverage**~~ — resolved in Phase 11: 11 languages via the generic `tree-sitter-tags` mechanism, honestly tiered by call-graph fidelity.
- ~~**Embedding endpoint availability**~~ — resolved in Phase 12: a down/misconfigured endpoint returns a specific, actionable error (not a crash or a hang) both at query time and mid-reindex, verified directly against a real unreachable-endpoint test.
- ~~**Remote embedding endpoint = network exposure of code**~~ — resolved in Phase 7: `Config::embeddings_policy()` refuses a non-loopback/non-private endpoint unless `allow_remote = true` is set explicitly, both in `config.toml` directly and via the GUI's Config tab checkbox.
- ~~**File watcher cost on large repos**~~ — resolved: the live auto-sync watcher (added after the original 10 phases, see below) debounces at 2s and filters obvious noise directories (`.git`, `target`, `node_modules`, `.nexuscontext`) before triggering a reindex. Extended in Phase 18 for cost *at scale* (many registered projects, not just one large one): a project not queried in `warm_window_secs` (6h default) stops being actively watched at all, with a synchronous catch-up reindex the next time it's actually queried. Phase 19 closed the remaining gap on a *single* actively-watched large project: `notify`'s Linux backend watches file opens, not just writes, so a reindex's own reads of every source file were re-triggering the next reindex indefinitely - fixed by unwatching for the duration of each reindex and giving the gap before the next attempt (`MIN_REINDEX_GAP`, 180s) enough margin to absorb the watch's own re-registration noise.
- **GNOME extension version churn** — GNOME Shell extensions frequently break across major GNOME releases; treat Phase 4 as low-priority/optional and keep it thin enough to be cheap to fix.
- **Graph incremental-update correctness** — on file change, edges referencing the changed file (e.g. `CALLS` into a renamed function) must be retracted and rebuilt, not just the file's own nodes appended. Worth a full-reindex fallback if incremental graph diffing gets too complex early on.
- **Bundled vs. network embedding model** — an alternative worth keeping in mind is embedding a small model directly in the binary (zero external process, at the cost of a fixed model). We're deliberately choosing the opposite tradeoff — a configurable external endpoint gives model choice and reuse of whatever's already running, at the cost of "semantic search needs something else up." Worth revisiting only if "zero external dependencies" becomes a hard requirement later; the graph layer already covers the tool's core value without embeddings either way.
