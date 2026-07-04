# NexusContext

**Objective:** A self-hosted, lightweight binary daemon that provides a standardized MCP interface for local codebase indexing, structural code intelligence (knowledge graph), semantic search, and RAG-based LLM orchestration — with a native Linux desktop GUI on top.

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
                     │   - chunking                  │
                     │                              │
                     │  Knowledge Graph (SQLite)      │
                     │   - nodes/edges, Cypher-lite   │
                     │                              │
   Embedding ────────┤  Embedding Pipeline           │
   endpoint (net)    │   - optional, off by default  │
                     │                              │
                     │  Vector Store (not built yet)  │
                     │                              │
                     │  RAG / Query Planner          │
                     │                              │
   GUI / extension ──┤  Control API (Unix socket)    │
                     └─────────────────────────────┘
```

Two transports, two purposes:
- **Stdio JSON-RPC** — reserved for MCP clients (IDE extensions, CLI agents). This is the actual MCP spec transport and shouldn't be shared with anything else.
- **Local Unix domain socket** — a separate control/status API for the GUI and GNOME extension (indexing progress, watched paths, config, ad-hoc search). Keeps the GUI decoupled from whatever MCP client happens to be attached to stdio at the time.

The daemon runs as a `systemd --user` service, independent of any GUI. The GUI is a client, not a requirement — the tool must be fully usable headless.

## 2. Component Breakdown

**Ingestion Engine**
- Directory watcher (`notify` crate), git-diff-aware: on file change, re-parse only the changed files rather than polling everything.
- Tree-sitter parsers per language, extracting functions/classes/interfaces as node boundaries instead of naive line-splitting.
- **Layered ignore rules**: hardcoded patterns (`.git`, `node_modules`, build dirs) → `.gitignore` hierarchy → project-specific `.nexusignore` (gitignore syntax) for one-off excludes. Symlinks always skipped.
- Incremental re-indexing — only re-parse/re-embed changed nodes, not whole files, on save.

**Knowledge Graph Layer** *(new — the main structural addition over the original proposal)*
- Every ingested file becomes graph nodes (`File`, `Function`, `Class`, `Interface`, `Route`, ...) linked by edges (`CALLS`, `IMPORTS`, `IMPLEMENTS`, `DEFINES`, `HTTP_CALLS`) derived straight from the tree-sitter AST — no embeddings involved.
- Stored in SQLite (not LanceDB) at `~/.local/share/nexuscontext/<project-hash>/graph.db` — cheap, embeddable, and a natural fit for graph traversal queries via recursive CTEs or a small Cypher-lite query layer.
- This is what makes `trace_call_path`, `get_architecture`, `detect_changes` (git-diff → affected-symbols mapping), and dead-code detection possible **without any embedding backend running at all** — directly relevant to keeping Ollama/embeddings optional rather than load-bearing.
- Semantic search becomes one additional signal layered on top of the graph, not the only retrieval mechanism.

**Embedding Pipeline** *(optional layer — daemon is fully useful without it)*
- No embedding runtime is bundled or hardwired. The daemon speaks the **OpenAI-compatible `/v1/embeddings` API** — the de facto standard that Ollama, LM Studio, vLLM, and llama.cpp server all implement — over a plain configurable HTTP endpoint.
- Config (`[embeddings]` in `config.toml`): `endpoint` (URL, e.g. `http://localhost:11434/v1` or a LAN host), `model` (e.g. `nomic-embed-text`), optional `api_key` (blank for local servers that don't need one).
- Since `endpoint` is just a URL, "Ollama on this machine" and "Ollama/vLLM on another box on the network" are the same code path — no special-casing.
- Startup health check against the endpoint; if unreachable, the daemon logs a clear error and keeps running in a degraded state (search/MCP tools return an explicit "embedding backend unavailable" instead of crashing).
- Retry/timeout are config knobs, not assumptions — useful once the endpoint might be a network hop away rather than localhost.

**Vector Store**
- LanceDB, embedded, disk-backed at `~/.local/share/nexuscontext/<project-hash>/vectors/`.
- One table per indexed project/workspace, keyed by content hash to dedupe.
- **Post-write integrity check**: after indexing, compare persisted row count against the in-memory count; if it falls suspiciously short, report `status: "degraded"` from `index_status` instead of silently claiming success.

**MCP Server**
- `listTools` / `callTool` per spec, newline-delimited JSON-RPC 2.0 over stdio. Logging goes to stderr exclusively - stdout is reserved for the protocol stream.
- Structural tools (graph-backed, no embeddings required): `index_repository` (build/rebuild the graph for a path - the prerequisite for everything else), `search_graph`, `trace_call_path`, `get_architecture`, `detect_changes`, `get_file_context` (plain file/line-range read, no embeddings involved either), `detect_dead_code`, `search_code` (FTS5 over file content), `query_graph` (Cypher-lite), `delete_project`.
- Retrieval tools (embedding-backed, degrade gracefully with a clear error if no endpoint configured): `search_codebase` (semantic), `query_memory`.
- `query_planner` tool decides file-read vs. graph search vs. keyword-fallback-graph-search (semantic search once the embeddings pipeline exists) to cut token spend - see Phase 5 for the honest version of what it does today.

**Control API (for GUI/extension, not MCP)**
- Unix socket, same JSON-RPC framing for consistency, but a distinct method namespace (`status.*`, `config.*`, `search.adhoc`).
- Exposes: indexing status/progress, per-project stats, config get/set, manual reindex trigger, ad-hoc search for the GUI's own search box.

**Desktop GUI — "NexusContext Manager"**
- GTK4 + `libadwaita` via `gtk-rs`, native Ubuntu/GNOME look, no Electron overhead.
- Views:
  - **Dashboard** — daemon status, projects indexed, auto-sync watcher count, last reindex time.
  - **Projects** — index/reindex/delete a project, see node/edge counts per project.
  - **Search** — ad-hoc structural query box with code-preview results (this is the main reason a GUI is worth building at all — trying queries without an agent in the loop).
  - **Architecture** — node/edge counts, index freshness, busiest files, language breakdown by file extension.
  - **Config** — embeddings endpoint/model, and the `allow_remote` opt-in for non-loopback endpoints.
  - **Logs** — tail of daemon logs for troubleshooting.
- Talks to the daemon exclusively over the control socket. Never touches stdio.
- Not required to be running for the daemon or MCP integrations to work — it's a management/inspection tool.

**GNOME Shell Extension (optional, thin)**
- Deliberately minimal: a top-bar indicator only.
  - Icon changes state (idle / indexing / error).
  - Dropdown: quick stats + a "Search…" entry that either does an inline quick lookup or launches the full GTK4 app.
- Runs inside `gnome-shell`'s process (GJS) — this is why it must stay thin. Anything heavier belongs in the GTK4 app, not the extension: Shell extensions that do real work are a common source of Shell crashes and are the most likely part of this stack to break across GNOME version upgrades.

## 3. Technical Stack

| Concern | Choice |
|---|---|
| Daemon language | Rust |
| Knowledge graph | SQLite, WAL mode (nodes/edges, FTS5 for content search, Cypher-lite traversal) |
| Vector engine | **Not implemented** — `search_codebase`/`query_memory` are stubs pending an embeddings HTTP client (see Phase 5); the original proposal's "LanceDB" pick was aspirational and never actually built, corrected here rather than left stale |
| Parsing | tree-sitter (Rust, Python) |
| Embeddings | OpenAI-compatible `/v1/embeddings` over configurable HTTP endpoint (Ollama, LM Studio, vLLM, llama.cpp server, local or LAN) — optional, daemon is useful without it; policy-gated (loopback/private by default, `allow_remote` opt-in for anything else) |
| MCP transport | JSON-RPC 2.0 over stdio |
| GUI/control transport | JSON-RPC 2.0 over Unix domain socket |
| GUI toolkit | GTK4 + libadwaita (`gtk-rs`) |
| Shell integration | GNOME Shell extension (GJS), status-only |
| Auto-sync | `notify-debouncer-mini`, 2s debounce, `serve`-mode only |
| Config | TOML, `~/.config/nexuscontext/config.toml` + env var overrides (`NEXUS_CACHE_DIR`, `NEXUS_LOG_LEVEL`, `NEXUS_LOG_FORMAT=json`) |
| Data dir | `~/.local/share/nexuscontext/` |
| Service management | `systemd --user` unit, autostart |
| Logging | `tracing` crate; `NEXUS_LOG_FORMAT=json` for structured/machine-parseable logs, plain text by default |

## 4. Full Roadmap

**Phase 0 — Scaffolding**
Cargo workspace with `nexusd` (daemon), `nexus-cli` (manual indexing/query CLI), and later `nexus-gui` as separate crates sharing a `nexus-core` lib.

**Phase 1 — Context-Aware Core**
Tree-sitter watcher, knowledge graph construction (nodes/edges in SQLite), CLI for manual reindex and graph queries. Embedding pipeline against a configurable endpoint is additive here, not a blocker for the rest of Phase 1.

**Phase 2 — MCP Implementation** ✅ *(vertical slice done)*
`listTools`/`callTool` over stdio; `index_repository`, `search_graph`, `trace_call_path`, `get_file_context`, `get_architecture`, `detect_changes` all working end-to-end (verified by piping real JSON-RPC messages into the binary, including a `detect_changes` run against this repo's own uncommitted diff). `search_codebase`/`query_memory` correctly degrade with a clear error, since the embeddings pipeline isn't built yet. Remaining: verify against an actual IDE client (e.g. Claude Code, Continue) rather than hand-crafted JSON-RPC. Stretch goal: an `install` subcommand that auto-detects installed agents and wires MCP config for each, rather than requiring manual `.mcp.json` edits.

**Phase 3 — Control API + Desktop GUI** ✅ *(vertical slice done)*
`nexusd` gained explicit `mcp`/`serve` subcommands to resolve the tension between MCP's per-session stdio transport and an always-on daemon. `serve` hosts the Unix-socket control API (`status.get`, `projects.list`/`reindex`, `config.get`/`set`, `search.adhoc`) and now logs to a file instead of stderr, since the GUI's Logs view needs something to tail. The GTK4/libadwaita app (`nexus-gui`) has all five views (Dashboard, Projects, Search, Config, Logs) wired to the control socket and was verified running against a real desktop session. Remaining: exercise the interactive paths (button clicks) rather than just the auto-load-on-open calls, and replace the deprecated `ViewSwitcherTitle` with the `AdwBreakpoint`-based pattern libadwaita 1.4+ recommends.

**Phase 4 — GNOME Shell Integration** ✅ *(vertical slice done)*
`extension/nexuscontext@nexuscontext.local/` - a top-bar icon polling `status.get` over the control socket every 15s, showing project count or a clear "not reachable" state, plus a menu item that launches the GTK4 app via `Gio.Subprocess`. Uses the modern ESM extension format (GNOME 45+, targets 45-50). Validated statically - `gnome-extensions pack` accepts the metadata/structure, and `gjs -m` confirms the JS parses cleanly (it only fails at the expected point, resolving `resource:///org/gnome/shell/...`, which only exists inside a running Shell process). Not yet loaded into a live Shell session: doing that requires a full Shell restart, which under Wayland means logging out, so live verification is deferred to whenever that's convenient rather than forced mid-session.

**Phase 5 — Agentic Intelligence & Caching** ✅ *(vertical slice done, scope narrowed to what's actually ours to build)*
`query_planner` MCP tool: a named file wins outright (`file_read`), a single identifier-like token goes straight to `search_graph` (`graph_search`), and a descriptive multi-word query gets a naive per-word graph search (`keyword_fallback_graph_search`) - the true semantic-search arm doesn't exist yet since there's no embedding pipeline, so the tool says so explicitly (`embeddings_configured` + a note) rather than pretending. On caching: we don't control the calling agent's LLM-side prompt cache, so "prefix caching for system prompts" isn't ours to implement directly - what we built instead is an in-process cache for `get_architecture`, keyed on the project's `last_indexed_unix`, so repeated calls against an unchanged index skip SQLite entirely and a reindex busts the cache automatically. Verified: all three planner strategies return correct results, and the cache shows miss→hit→(miss after reindex) exactly as expected.

**Phase 6 — Packaging & Distribution** ✅ *(`.deb` + systemd unit done and verified; Flatpak is manifest-only; extensions.gnome.org submission is a manual step)*
- `.deb` (via `cargo-deb`, config in `crates/nexusd/Cargo.toml`): bundles `nexusd` + `nexus` + the systemd user unit + README. Built, installed via `dpkg -i`, verified the real installed binaries and the vendor-shipped unit at `/usr/lib/systemd/user/nexuscontext.service` both work end-to-end, then cleanly removed via `dpkg -r`.
- `packaging/systemd/nexuscontext.service`: hardened user unit (`ProtectSystem=strict`, `ProtectHome=read-only`, `RuntimeDirectory=nexuscontext` for the control socket, explicit `ReadWritePaths` for config/data) - live-tested standalone before folding into the `.deb`, including confirming the hardening doesn't break functionality.
- `packaging/flatpak/org.nexuscontext.Manager.json`: manifest only, not built - the GNOME Platform+SDK runtimes are a ~1.5-2GB download, so building was deliberately deferred rather than pulling that into this environment. Needs a generated Cargo vendor file (`flatpak-cargo-generator.py`) and an actual app icon before it would build/pass Flathub review - both noted in `packaging/flatpak/README.md`.
- GNOME extension submission to extensions.gnome.org: a manual, account-based review process on a third-party site - not something to automate. The extension itself is packaged and ready (see Phase 4); submitting it is a step for whoever owns that decision.

**Phase 7 — Hardening & Docs** ✅ *(vertical slice done)*
`Config::embeddings_policy()` refuses to use a non-loopback/non-private embeddings endpoint unless `allow_remote = true` is set explicitly - verified blocking a remote endpoint, then unblocking it with the opt-in. `Config::allowed_roots` (opt-in, empty by default) restricts `index_repository`/reindex to specific directories, enforced once in `nexus_index::index_project` so it applies regardless of caller (CLI/MCP/control API) - verified both the allow and refuse paths. `NEXUS_LOG_FORMAT=json` gives structured logs in both `mcp` and `serve` modes. `INSTALL.md` documents the real, working install/usage flow end-to-end (build, `.deb` install, systemd unit, MCP client config, CLI, GUI, GNOME extension, config options) rather than the aspirational version.

**Phase 8 — Team-Shared Index Artifact** ✅ *(vertical slice done, scope narrowed to what's honest given no incremental indexing)*
`nexus export <path>` zstd-compresses the local graph into `.nexuscontext/index.db.zst` (level 9) and adds a `merge=ours` `.gitattributes` line so the binary artifact doesn't cause merge conflicts. `nexus import <path>` decompresses it straight into place and updates the registry from the imported DB's real stats - skipping the tree-sitter walk entirely. Since there's no incremental diffing yet (an open risk since Phase 1), this only saves the *first* reindex, not ongoing syncing - reframed from the original "bootstrap + incremental diff" wording to match what's actually built. Never committed unless the user runs `export` themselves. Verified: exported a real index, imported it into a from-scratch data dir simulating a teammate's clone, and successfully ran `search-graph` against the imported data with zero reindexing.

**Phase 9 — Obsidian-Compatible Markdown Export** ✅ *(vertical slice done; corrected scope below)*
`nexus export <path> --format obsidian` writes one `.md` file per function/type into `.nexuscontext/vault/`, each with a `## Calls` / `## Called by` section of `[[wikilinks]]` derived from the real `CALLS` edges - a valid Obsidian vault with zero integration code, since a vault is just markdown files. (Correction from the original wording here: this covers the graph only, not "ADRs" - there's no ADR management tool in this project, so there was nothing to export there; that line overclaimed scope that was never built.) Verified: exported the fixture project, confirmed every `[[wikilink]]` in the vault resolves to an actual file, and fixed a real duplicate-link bug along the way (a caller invoking the same callee twice produced the link twice, now deduped). Static/point-in-time by design - the GTK4 GUI still owns anything needing live daemon state.

**Phase 10 — Feature Gap Closure** ✅ *(done - a round of work closing gaps found by comparing against [DeusData/codebase-memory-mcp](https://github.com/DeusData/codebase-memory-mcp), in priority order)*
- **Dead-code detection** — `detect_dead_code` (MCP tool + `nexus dead-code`): functions with no inbound `CALLS` edge, excluding `main`. Inherits the same-name-resolution caveats below.
- **CLI/MCP parity** — `get_architecture`, `detect_changes`, `query_planner` were MCP-only; extracted the actual logic into `crates/nexus-index/src/queries.rs` so `nexus architecture`/`detect-changes`/`query-planner` reach it too, instead of duplicating it.
- **`delete_project`** — removes a project's graph + registry entry (not the source) via MCP tool, `nexus delete`, the control API, and a Delete button in the GUI's Projects view.
- **GUI Architecture tab** — the GUI had no view onto `get_architecture` at all. New tab: node/edge counts, index freshness, busiest files, and a new language breakdown (file counts per extension).
- **Live file-watching auto-sync** — a `notify-debouncer-mini` watcher in `serve` mode, 2s debounce, syncs against the registry every 30s to pick up newly-indexed projects, filters obvious noise directories (`.git`, `target`, `node_modules`, `.nexuscontext`). `serve`-mode only - `mcp` sessions don't own background threads. Watched-project count surfaced through `status.get` and the GUI Dashboard.
- **Multi-agent auto-install** — `nexus install` shells out to `claude mcp add` for Claude Code (the exact mechanism already proven in this project's own setup) and does a merge-safe write to Claude Desktop's `claude_desktop_config.json`. Anything else gets a generic `mcpServers` snippet printed rather than a guessed config format.
- **Full-text code search** — `search_code` (MCP tool + `nexus search-code`): SQLite FTS5 over indexed file content, not just symbol names. Query is matched as a literal phrase. Only covers files tree-sitter already parses (Rust/Python), not every file in the repo.
- **Cross-file call resolution** — the biggest one. `index_directory` now runs in two passes: nodes first, then a project-wide name registry resolves call sites across file boundaries once every file is known. Same-file matches still win; a cross-file match resolves only when the callee name is unique project-wide, so ambiguous same-named functions across files stay unresolved rather than guessing wrong. Not import-aware (no `use`/`import` parsing) - name-based only.
- **Cypher-lite ad-hoc query** — `query_graph` (MCP tool + `nexus query-graph`): deliberately minimal, one pattern shape (`MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE a.name = 'x'] RETURN a|b`), not a real Cypher implementation. Anything else fails with a clear "unsupported" error.
- **WAL mode** — `journal_mode=WAL` on every graph.db, so `nexusd serve` and `nexusd mcp` can hold concurrent connections to the same graph without one locking out the other.
- **A real concurrency bug, found and fixed** — the two-pass cross-file resolution widened the window where two full-rebuilds of the same project (e.g. the watcher firing during a manual reindex) could interleave and produce a dangling foreign key. `index_directory` now runs inside `BEGIN IMMEDIATE`/`COMMIT` with a 30s busy timeout, so a second rebuild blocks until the first commits instead of corrupting it. Verified by intentionally reproducing the race.
- **GUI window-stacking bug, found and fixed** — `connect_activate` was rebuilding an entire new window every time GNOME Shell re-launched the already-running app (app grid, search); it now checks for an existing window and presents that instead.
- **`allow_remote` gap, found and fixed** — the Phase 7 policy field existed in `Config` but the control API's `config_set` never read it, and the GUI's Config tab had no checkbox for it - anyone with a legitimate non-loopback endpoint (e.g. a Tailscale-hosted Ollama) would hit `RemoteBlocked` with no way to opt in short of hand-editing `config.toml`. Both fixed.

## 5. Why This Counts as "Full-Fledged"

A daemon alone is a backend, not a tool. What makes this complete for a Linux desktop user:
- Headless-first: daemon + MCP server work with zero GUI, so IDE/agent integration isn't blocked on the GUI being built.
- A real inspection/management surface (GTK4 app) for the parts a CLI is bad at — browsing search results, seeing indexing status at a glance.
- Desktop-native integration (top-bar status) without over-investing in Shell extension surface area, which is the most fragile part of any GNOME-integrated tool.
- Proper packaging (.deb/Flatpak + systemd unit) so it installs and runs like a normal Ubuntu service, not a script someone has to remember to start.

## 6. Open Risks / Decisions to Revisit

- ~~**LanceDB Rust binding maturity**~~ — moot: the vector-store pick was never revisited once the knowledge graph proved sufficient for every structural tool built so far (full-text search also landed on SQLite FTS5 rather than a separate vector engine). Revisit only once the embeddings pipeline actually gets built.
- **Tree-sitter grammar coverage** — decide initial supported-language list; unsupported files fall back to naive chunking.
- **Embedding endpoint availability** — daemon must degrade gracefully (clear error, not crash) if the configured embedding endpoint is unreachable, whether that's localhost Ollama or a remote box.
- ~~**Remote embedding endpoint = network exposure of code**~~ — resolved in Phase 7: `Config::embeddings_policy()` refuses a non-loopback/non-private endpoint unless `allow_remote = true` is set explicitly, both in `config.toml` directly and via the GUI's Config tab checkbox.
- ~~**File watcher cost on large repos**~~ — resolved: the live auto-sync watcher (added after the original 10 phases, see below) debounces at 2s and filters obvious noise directories (`.git`, `target`, `node_modules`, `.nexuscontext`) before triggering a reindex.
- **GNOME extension version churn** — GNOME Shell extensions frequently break across major GNOME releases; treat Phase 4 as low-priority/optional and keep it thin enough to be cheap to fix.
- **Graph incremental-update correctness** — on file change, edges referencing the changed file (e.g. `CALLS` into a renamed function) must be retracted and rebuilt, not just the file's own nodes appended. Worth a full-reindex fallback if incremental graph diffing gets too complex early on.
- **Bundled vs. network embedding model** — an alternative worth keeping in mind is embedding a small model directly in the binary (zero external process, at the cost of a fixed model). We're deliberately choosing the opposite tradeoff — a configurable external endpoint gives model choice and reuse of whatever's already running, at the cost of "semantic search needs something else up." Worth revisiting only if "zero external dependencies" becomes a hard requirement later; the graph layer already covers the tool's core value without embeddings either way.
