# Installing and Using NexusContext

This covers what's actually built and working today (all 10 phases in `README.md`, including the Phase 10 feature-gap-closure round). It assumes Ubuntu/GNOME.

## 1. Build from source

Requires Rust (stable) and, only for the GUI, GTK4 + libadwaita dev headers:

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev pkg-config build-essential
cargo build --release
```

This produces `target/release/{nexusd, nexus, nexuscontext-gui}`.

## 2. Install the daemon + CLI (`.deb`)

```bash
cargo install cargo-deb
cargo deb -p nexusd --no-build   # after `cargo build --release` above
sudo dpkg -i target/debian/nexuscontext_*.deb
```

This installs `nexusd` and `nexus` to `/usr/bin/`, and the systemd user unit to `/usr/lib/systemd/user/nexuscontext.service`.

## 3. Run the daemon

**As a background service** (for the GUI/GNOME extension to talk to):

```bash
systemctl --user enable --now nexuscontext.service
systemctl --user status nexuscontext.service
```

**As an MCP server** (what your IDE/agent should launch, *not* what you run by hand): configure your MCP client to run `nexusd mcp` as a subprocess. For Claude Code, add to `.mcp.json`:

```json
{
  "mcpServers": {
    "nexuscontext": {
      "command": "nexusd",
      "args": ["mcp"]
    }
  }
}
```

## 4. Index a project and try the CLI

```bash
nexus reindex /path/to/your/project
nexus search-graph SomeFunctionName --project /path/to/your/project
nexus trace SomeFunctionName --project /path/to/your/project --direction inbound
nexus architecture --project /path/to/your/project      # node/edge counts, busiest files, languages
nexus dead-code --project /path/to/your/project          # functions with no inbound calls
nexus search-code "some literal text" --project /path/to/your/project   # full-text, not just symbol names
nexus detect-changes --project /path/to/your/project      # uncommitted git diff -> affected symbols
nexus query-planner "some question" --project /path/to/your/project     # picks file-read vs graph-search vs keyword-fallback
nexus query-graph "MATCH (f:File)-[:DEFINES]->(fn:Function) WHERE f.name = 'main.rs' RETURN fn" --project /path/to/your/project
nexus delete /path/to/your/project                        # remove a project's index (not its source)
nexus status
```

Reindexing is safe to run concurrently (e.g. while the auto-sync watcher is also active) - `index_directory` runs inside a transaction with a busy timeout, so a second rebuild waits for the first instead of corrupting the graph.

**Sharing an index with teammates** (skips the first reindex on their end):

```bash
nexus export /path/to/your/project   # writes .nexuscontext/index.db.zst next to source
# ... teammate clones the repo, then:
nexus import /path/to/their/checkout
```

**Browsing the graph in Obsidian** (optional):

```bash
nexus export /path/to/your/project --format obsidian   # writes .nexuscontext/vault/*.md
```

Open `.nexuscontext/vault/` as an Obsidian vault to browse functions/types and their call relationships via the graph view.

**Auto-configuring an MCP agent instead of hand-editing `.mcp.json`:**

```bash
nexus install
```

Detects Claude Code (via its own `claude mcp add` CLI) and Claude Desktop (merges into `claude_desktop_config.json` without touching anything else already in it). Prints a generic `mcpServers` snippet for anything else, rather than guessing at a config format it can't verify.

## 5. MCP tools available to agents

Once `nexusd mcp` is wired into an agent, these tools are exposed (no embeddings/network required for any of them except the last two, which are stubbed pending an embedding pipeline):

`index_repository`, `search_graph`, `trace_call_path`, `get_file_context`, `get_architecture`, `detect_changes`, `detect_dead_code`, `search_code`, `query_graph`, `query_planner`, `delete_project`, `search_codebase`, `query_memory`.

## 6. Desktop GUI

```bash
nexuscontext-gui
```

Requires `nexusd serve` (the systemd unit above) to be running - the GUI is a client of the control socket, not a standalone tool. Six tabs: Dashboard (status + auto-sync watcher count), Projects (index/reindex/delete), Search, Architecture (node/edge counts, busiest files, language breakdown), Config, Logs.

## 7. GNOME Shell extension (optional)

```bash
cp -r extension/nexuscontext@nexuscontext.local ~/.local/share/gnome-shell/extensions/
```

New extensions require a Shell restart to be picked up (log out/in on Wayland). After that:

```bash
gnome-extensions enable nexuscontext@nexuscontext.local
```

Shows a top-bar icon with daemon status and a launcher for the GUI.

## 8. Configuration

`~/.config/nexuscontext/config.toml` (created on demand, everything below is optional):

```toml
[embeddings]
endpoint = "http://localhost:11434/v1"   # OpenAI-compatible; Ollama, LM Studio, vLLM, etc.
model = "nomic-embed-text"
allow_remote = false   # must be true to use a non-loopback/private endpoint

allowed_roots = []   # if non-empty, index_repository/reindex refuses paths outside these
```

Env var overrides: `NEXUS_CACHE_DIR` (data dir), `NEXUS_LOG_LEVEL` (`trace`/`debug`/`info`/`warn`/`error`), `NEXUS_LOG_FORMAT=json` (structured logs, `serve`/`mcp` modes both support it).

`allow_remote` can also be set from the GUI's Config tab (a checkbox), not just by hand-editing `config.toml`.

## Known limitations (see `README.md` for full detail)

- Semantic search (`search_codebase`, `query_memory`) is not implemented yet - structural tools work fully without it. There's no vector store either (the original proposal's LanceDB pick was never actually built).
- Call resolution is name-based, not import-aware: same-file matches win, and a cross-file call resolves only when the callee name is unique project-wide. Two files defining the same-named function, with no local match in the caller's file, stays unresolved rather than guessing wrong.
- Reindexing is a full rebuild, not an incremental diff (though concurrent rebuilds of the same project are now safe - see above).
- `query_graph`'s Cypher-lite supports exactly one pattern shape (`MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...] RETURN a|b`) - not a real query language.
- `search_code`'s full-text index only covers files tree-sitter already parses (Rust/Python), not every file in the repo.
- The Flatpak manifest (`packaging/flatpak/`) hasn't been built - see its README for the remaining steps.
