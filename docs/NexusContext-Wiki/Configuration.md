# Configuration

`~/.config/nexuscontext/config.toml` — created on demand, everything below
is optional. Missing entirely is not an error; every field has a
zero-config default. Written owner-only (`0600`) on save — see
[[Security-Model]].

```toml
allowed_roots = []   # if non-empty, index_repository/reindex/get_file_context/
                      # detect_changes refuse any path outside these roots

[embeddings]
enabled = false
endpoint = "http://localhost:11434/v1"   # OpenAI-compatible
model = "nomic-embed-text"
api_key = ""          # optional; never echoed back over the control socket
timeout_secs = 30
allow_remote = false  # must be true to use a non-loopback/private endpoint

[watcher]
warm_window_secs = 21600   # 6h default - see Watcher-and-Freshness

[tools]
preset = "standard"   # "minimal" (5) | "standard" (default, 10) | "full" (14)
# enabled = ["search_code", "get_architecture"]   # explicit list, overrides preset

[lsp]
enabled = false                  # opt-in - see Security-Model for what turning this on means
server_command = "rust-analyzer" # Rust only for now - see below
max_concurrent_servers = 2
request_timeout_secs = 10
```

## Field-by-field

- **`allowed_roots`** — empty (default) means unrestricted, matching the
  "useful with zero config" goal. An opt-in safety rail, not a default
  restriction. See [[Security-Model]] for exactly which tools it gates.
- **`[embeddings]`** — the whole section is optional and off by default;
  see [[Embeddings-and-Semantic-Search]] for the full policy model
  (`NotConfigured`/`Disabled`/`RemoteBlocked`/`Allowed`).
- **`[watcher].warm_window_secs`** — how long a project stays "warm"
  (actively watched, auto-reindexed on change) after it was last queried.
  See [[Watcher-and-Freshness]].
- **`[tools]`** — which of the 14 MCP tools get advertised to a calling
  agent. See [[MCP-Tools]] for the preset breakdown.
- **`[lsp]`** — off by default. When on, `index_repository`'s `deep`
  argument (or `nexus reindex --deep`) spawns `server_command` to resolve
  cross-file references `rust-analyzer`-side, adding `CALLS_RESOLVED`
  edges alongside the static graph - never on the ordinary auto-reindex
  path, never load-bearing for any tool. Rust-only pilot; see
  [[Storage-and-Data-Model]] for the edge kind and [[Security-Model]] for
  why this is the one config section that runs an external binary rather
  than just connecting to one.

## Env var overrides

- `NEXUS_CACHE_DIR` — overrides the data dir (`~/.local/share/nexuscontext`
  by default).
- `NEXUS_LOG_LEVEL` — `trace`/`debug`/`info`/`warn`/`error`.
- `NEXUS_LOG_FORMAT=json` — structured, machine-parseable logs; plain text
  is the default. Works for both `serve` and `mcp` modes.

## Editing without hand-editing the file

The GUI's Config tab exposes all four `[embeddings]` fields plus a "Test
Connection" button (embeds a short probe string, reports back
model/dimension/latency). From the CLI: `nexus test-embeddings` (global
check) and `nexus search-codebase <query> --project <path>`. The control
API's `config.get`/`config.set` back both — see [[Security-Model]] for how
`api_key` is handled across that boundary.

## Related

[[Security-Model]] · [[Embeddings-and-Semantic-Search]] ·
[[Watcher-and-Freshness]] · [[MCP-Tools]] · [[GUI-and-Extension]]
