# CLI Reference

The `nexus` binary — a first-class interface (not a debug side door),
routed through the exact same shared code as the MCP tools and the control
API (`touch_and_catchup` and friends — see [[Watcher-and-Freshness]]), so
CLI usage keeps a project warm and self-heals from cold exactly like an
agent session does.

## Everyday commands

```bash
nexus reindex /path/to/project              # build or rebuild the graph
nexus reindex /path/to/project --deep       # + LSP-resolved-symbol enrichment (Rust, opt-in)
nexus search-graph SomeFunction --project /path/to/project
nexus trace SomeFunction --project /path/to/project --direction inbound
nexus architecture --project /path/to/project
nexus dead-code --project /path/to/project
nexus dead-code --project /path/to/project --path-prefix pkg/events  # scope to a subdirectory (monorepos)
nexus search-code "some literal text" --project /path/to/project
nexus detect-changes --project /path/to/project    # uncommitted git diff -> affected symbols
nexus query-planner "some question" --project /path/to/project
nexus query-graph "MATCH (a:Function)-[:CALLS]->(b:Function) RETURN b" --project /path/to/project
nexus delete /path/to/project                       # remove a project's index, not its source
nexus status
```

Reindexing is safe to run concurrently with the background auto-sync
watcher — see [[Indexing-Pipeline]] for the transaction/locking that makes
that true.

## Sharing an index

```bash
nexus export /path/to/project     # writes .nexuscontext/index.db.zst
nexus import /path/to/checkout    # a teammate skips the first reindex
```

## Browsing the *code* in Obsidian (a different vault from this one)

```bash
nexus export /path/to/project --format obsidian   # writes .nexuscontext/vault/*.md
```

One note per function/type/section, each with its actual source snippet
(syntax-highlighted) plus `[[wikilinks]]` derived from the real call graph.
Open `.nexuscontext/vault/` as its own separate Obsidian vault — this is
generated, per-project, local-only (gitignored), and answers "what does
this codebase's call graph look like," not "how does NexusContext itself
work" (that's what *this* vault, `docs/NexusContext-Wiki/`, is for).

## Auto-configuring an MCP agent

```bash
nexus install
```

Detects Claude Code (via its own `claude mcp add` CLI) and Claude Desktop
(merges into `claude_desktop_config.json` without touching anything else
already there). Resolves Claude Desktop's config path correctly on all
three platforms (`~/.config`, `~/Library/Application Support`, `%APPDATA%`
via `directories::BaseDirs` — previously hardcoded to the Linux path only).
Prints a generic `mcpServers` snippet for anything else, rather than
guessing at a config format it can't verify.

The CLI itself runs identically everywhere - Linux, macOS, and Windows,
x86_64 and arm64 - since none of it depends on `nexusd serve`. See
[[Known-Limitations]] for what's platform-tiered (the daemon/control API,
not the CLI or MCP tools).

## Related

[[MCP-Tools]] · [[Watcher-and-Freshness]] · [[Storage-and-Data-Model]]
