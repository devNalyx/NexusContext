# Storage and Data Model

## On-disk layout

| Path | What lives there |
|---|---|
| `~/.config/nexuscontext/config.toml` | Configuration — see [[Configuration]]. Created on demand; owner-only (`0600`) permissions, see [[Security-Model]]. |
| `~/.local/share/nexuscontext/` | Data dir (overridable via `NEXUS_CACHE_DIR`) |
| `~/.local/share/nexuscontext/<project-hash>/graph.db` | One SQLite database per indexed project. |
| `~/.local/share/nexuscontext/projects.json` | The project registry — see below. |
| `~/.local/share/nexuscontext/usage_stats.json` | Lifetime aggregate call/latency/output-size counters, see below. |
| `~/.local/share/nexuscontext/nexusd.log` | `serve`-mode daemon log (file-backed, not stderr — the GUI's Logs tab tails this directly). |
| `$XDG_RUNTIME_DIR/nexuscontext/nexuscontext.sock` | The control API Unix socket. |
| `<repo>/.nexuscontext/index.db.zst` | An optional, explicitly-exported shareable index snapshot (see below) — never written unless `nexus export` is run. |
| `<repo>/.nexuscontext/vault/` | An optional, explicitly-exported Obsidian-compatible browsable vault of the *code* (one note per function/type/section, with source + call graph) — a different thing from this documentation vault. Generated via `nexus export --format obsidian`. |

`<project-hash>` is a stable hash of the project's canonicalized root path
— computed once, used to namespace everything about that project under the
shared data dir without needing the human-readable path to be
filesystem-safe.

## The knowledge graph (`graph.db`)

SQLite, **WAL journal mode** — lets `nexusd serve` and a `nexusd mcp`
session hold concurrent connections to the same graph without one locking
out the other. Node kinds: `File`, `Function`, `Type`, `Section` (a
markdown heading — see [[Indexing-Pipeline]]). Edge kinds: `Defines`,
`Calls`, `Contains`, `CallsResolved` (`CALLS_RESOLVED` — an LSP-resolved
call edge, issue #10; only present after an explicit `deep` reindex with
`[lsp] enabled = true`, and always stored *alongside* whatever `Calls`
edges the static tree-sitter pass already found, never in place of them —
`trace_call_path`/`detect_dead_code` union both kinds when walking the
call graph, so a project that's never run a `deep` reindex behaves exactly
as before this existed). Full-text search via a parallel FTS5 table over
indexed file content. A minimal Cypher-lite layer answers the `query_graph`
tool's one supported pattern shape.

An `embeddings` table existed here through v0.1.17, storing embedding
vectors as plain BLOB rows for the now-removed optional semantic-search
layer — see [[ADRs/README|ADR 0010]]. `GraphStore::open` drops that table
(and its index) on any database that still has it, so upgrading an
existing install cleans up the old schema automatically; nothing needs to
be done by hand.

## The project registry (`projects.json`)

One `ProjectEntry` per indexed project: `root_path` (always absolute and
canonical — see [[Watcher-and-Freshness]]), a content hash, node/edge
counts, `last_indexed_unix`, `last_queried_unix` (the warm/cold signal),
and auto-reindex history (count/failures/timing, kept separate from
manual-reindex activity). Written via temp-file + atomic rename, not a
direct `fs::write` — two writers racing (the background watcher and a
manual reindex, say) can't interleave and truncate the file into something
unparseable.

## Usage stats (`usage_stats.json`)

Lifetime aggregate counters only — call count, error count, total/max
latency, total output bytes — per MCP tool and per control-API method, in
two separate buckets so agent-driven usage and GUI-driven usage aren't
conflated. Not a per-call event log or an audit trail; this is an
insight-gathering pass, not billing-adjacent. Surfaced via the control
API's `stats.get` and the GUI's Usage tab.

## Sharing an index without re-indexing

`nexus export <path>` zstd-compresses the local graph (level 9) into
`.nexuscontext/index.db.zst` next to the source, with a `merge=ours`
`.gitattributes` line so the binary artifact doesn't cause merge conflicts.
`nexus import <path>` decompresses it straight into place and updates the
registry from the imported DB's real stats, skipping the tree-sitter walk
entirely. Since there's no incremental diffing (see
[[Known-Limitations]]), this only saves the *first* reindex on the
teammate's end, not ongoing syncing. The decompress is streamed and capped
at 2GiB rather than done in one unbounded call — a crafted artifact from a
compromised or malicious teammate can't turn `import` into a
decompression-bomb disk-exhaustion vector; a refused import removes its
own partial output rather than leaving it on disk. See [[Security-Model]].

## Related

[[Architecture]] · [[Watcher-and-Freshness]] · [[Configuration]] ·
[[Security-Model]]
