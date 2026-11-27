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
preset = "standard"   # "minimal" (5) | "standard" (default, 13) | "full" (31)
# enabled = ["search_code", "get_architecture"]   # explicit list, overrides preset

[lsp]
enabled = false                  # opt-in - see Security-Model for what turning this on means
server_command = "rust-analyzer" # Rust only for now - see below
max_concurrent_servers = 2
request_timeout_secs = 10

# Opt-in YAML/JSON config indexing (issue #112). Off by default because
# config files often contain secrets. Exclusions are applied before reading.
[config_index]
enabled = false
include = ["**/*.yaml", "**/*.yml", "**/*.json"]
exclude = []             # wins over include; e.g. ["**/secrets/**"]
metadata_only = false    # true stores key paths/locations but no raw values

# Opt-in Obsidian-style vault metadata (issue #133). Off by default; a source
# registration never turns this on. See [[Obsidian-Secondbrain]].
[vault_index]
enabled = false          # true extracts wikilink targets, inline #tags and
                         # frontmatter tags/aliases identifiers with line
                         # numbers and resolution status only - no values

# Human-registered external knowledge sources (issue #103). Normally written
# by `nexus source add`, which canonicalizes root_path and indexes it at once.
[[sources]]
id = "secondbrain"
kind = "obsidian-vault"              # free-form; defaults to "markdown"
root_path = "/absolute/canonical/vault"  # must respect allowed_roots if set
resource = "obsidian-vault://secondbrain"  # default: <kind>://<id>
title = "Personal notes"             # optional
description = "Cross-project vault"  # optional
enabled = true                       # default true
```

## Field-by-field

- **`allowed_roots`** — empty (default) means unrestricted, matching the
  "useful with zero config" goal. An opt-in safety rail, not a default
  restriction. See [[Security-Model]] for exactly which tools it gates.
- **`[watcher].warm_window_secs`** — how long a project stays "warm"
  (actively watched, auto-reindexed on change) after it was last queried.
  See [[Watcher-and-Freshness]].
- **`[tools]`** — which of the 31 MCP tools get advertised to a calling
  agent. See [[MCP-Tools]] for the preset breakdown.
- **`[lsp]`** — off by default. When on, `index_repository`'s `deep`
  argument (or `nexus reindex --deep`) spawns `server_command` to resolve
  cross-file references `rust-analyzer`-side, adding `CALLS_RESOLVED`
  edges alongside the static graph - never on the ordinary auto-reindex
  path, never load-bearing for any tool. Rust-only pilot; see
  [[Storage-and-Data-Model]] for the edge kind and [[Security-Model]] for
  why this is the one config section that runs an external binary rather
  than just connecting to one.
- **`[config_index]`** — off by default. When on, `.yaml`/`.yml`/`.json`
  files are indexed for `search_code`, with source-linked key paths, document
  numbers, line ranges, and explicit parsed/partial/unsupported states.
  `include`/`exclude` are repository-relative globs; `exclude` wins and is
  applied before ingestion. `metadata_only = true` stores key paths and
  locations but not raw values — useful when values may contain secrets, but
  not secret detection or redaction. No Helm/template rendering, effective
  values, or network access. `parsed` means valid syntax plus complete mapping
  key paths (YAML flow mappings included); a quoted template-like string keeps
  its static keys with a `template_syntax` note, while unparseable template
  syntax is `unsupported` (#124). See [[Security-Model]] and
  [[Known-Limitations]].
- **`[vault_index]`** — off by default, deliberately separate from
  `[config_index]` and from source registration. When `enabled = true`, the
  Markdown ingest pass extracts only wikilink targets, inline `#tags`, and
  frontmatter `tags`/`aliases` identifiers, with line numbers and resolution
  status. Property values, callout bodies, embed contents, Canvas text,
  formulas and fetched targets are never stored. Reindex the vault after
  enabling it; `vault_context` errors clearly while it is disabled. See
  [[Obsidian-Secondbrain]] and ADR 0021.
- **`[[sources]]`** — human-registered external knowledge sources (e.g. an
  Obsidian vault). Written by `nexus source add`; the CLI canonicalizes the
  root, refuses duplicates and paths outside `allowed_roots`, and indexes
  immediately. `resource` is the OKF-style provenance URI usable by
  `nexus memory add --source`; registration is metadata only and never
  implies human approval of a fact. Agents see sources read-only through
  `list_sources`. Those `id`/`resource` values are what the explicit `sources`
  fan-out accepts. See [[CLI-Reference]], [[MCP-Tools]] and
  [[Obsidian-Secondbrain]].

## Explicit `sources` fan-out

Five read tools — `search_code`, `search_graph`, `get_file_context`,
`get_architecture` and `vault_context` — accept an optional `sources` array of
registered source IDs or resource URIs, e.g.
`"sources": ["secondbrain", "obsidian-vault://secondbrain"]`. It is mutually
exclusive with `repo_path` (use one or the other), capped at 16 entries, and
resolves only to currently enabled registered sources; unknown, duplicate or
disabled entries are errors rather than silent omissions. There is no new tool
name and no implicit federation: a call without `sources` stays scoped to one
`repo_path`.

The response is grouped, with each source's own provenance and freshness kept
attached to its `data`:

```json
{
  "source_count": 1,
  "selected_by": ["id"],
  "sources": [
    {
      "id": "secondbrain",
      "kind": "obsidian-vault",
      "resource": "obsidian-vault://secondbrain",
      "root_path": "/absolute/canonical/vault",
      "index_freshness": {
        "indexed": true,
        "mode": "stdio",
        "watcher_active": false,
        "warm": true,
        "note": "warm means recently indexed, not verified-current"
      },
      "data": { "...": "the tool's normal per-source result" }
    }
  ],
  "note": "Explicit fan-out only; no implicit federation. Each result is independently authorized and freshness-labelled."
}
```

See [[MCP-Tools]] and ADR 0022.

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
