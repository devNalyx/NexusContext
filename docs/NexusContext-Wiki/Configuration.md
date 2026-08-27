# Configuration

`~/.config/nexuscontext/config.toml` — created on demand, everything below
is optional. Missing entirely is not an error; every field has a
zero-config default. Written owner-only (`0600`) on save — see
[[Security-Model]].

```toml
allowed_roots = []   # if non-empty, every MCP tool/CLI command that takes a
                      # repo_path (index_repository, get_file_context, trace_call_path,
                      # search_graph, search_code, detect_changes, detect_dead_code,
                      # query_planner, query_graph, delete_project, export/import)
                      # refuses any path outside these roots

[watcher]
warm_window_secs = 21600   # 6h default - see Watcher-and-Freshness

[tools]
preset = "standard"   # "minimal" (5) | "standard" (default, 10) | "full" (12)
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
- **`[watcher].warm_window_secs`** — how long a project stays "warm"
  (actively watched, auto-reindexed on change) after it was last queried.
  See [[Watcher-and-Freshness]].
- **`[tools]`** — which of the 12 MCP tools get advertised to a calling
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
- `NEXUS_CONFIG_DIR` — overrides the config dir (`~/.config/nexuscontext` on
  Linux, `~/Library/Application Support/nexuscontext` on macOS, the roaming
  `AppData` known folder on Windows, by default) — where `config.toml` is
  read from. A plain env var read, not routed through the OS-specific
  `directories` crate, so it works identically cross-platform. Primarily for
  tests that need a controlled `allowed_roots` without depending on
  home-directory resolution (see `crates/nexus-index/tests/path_security.rs`).
- `NEXUS_LOG_LEVEL` — `trace`/`debug`/`info`/`warn`/`error`.
- `NEXUS_LOG_FORMAT=json` — structured, machine-parseable logs; plain text
  is the default. Works for both `serve` and `mcp` modes.

## Editing without hand-editing the file

The control API's `config.get`/`config.set` back the GUI's config-reading
views. There is no editable field routed through `config.set` today - it's
a reachable no-op round trip, kept as the place future GUI-editable
settings (e.g. `allowed_roots`) would hang off of.

## Related

[[Security-Model]] · [[Watcher-and-Freshness]] · [[MCP-Tools]] ·
[[GUI-and-Extension]]
