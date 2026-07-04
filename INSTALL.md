# Installing and Using NexusContext

This covers what's actually built and working today (all 13 phases in `README.md`). It assumes Ubuntu/GNOME.

## 0. Download a release (Linux/macOS)

Tagged releases publish real binaries via GitHub Actions - no toolchain needed. Grab the latest from the [Releases page](https://github.com/devNalyx/NexusContext/releases): a `.deb` or `.rpm` for Linux (full daemon/CLI/GUI), a plain `nexuscontext-linux-x86_64.tar.gz` for other distros, or `nexuscontext-macos-aarch64.tar.gz` for macOS (Apple Silicon; Intel Macs run this fine under Rosetta 2, and this is CLI + daemon only - no native GUI build for macOS). Unsigned macOS binaries need `xattr -d com.apple.quarantine <binary>` or a right-click-Open the first time, since they aren't notarized. Windows isn't published yet - see `README.md`'s Phase 13 for why.

Otherwise, build from source:

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

Once `nexusd mcp` is wired into an agent, these tools are exposed (no embeddings/network required for any of them except the last two, which need `embeddings.enabled = true` and a reachable endpoint/model configured - see Section 8):

`index_repository`, `search_graph`, `trace_call_path`, `get_file_context`, `get_architecture`, `detect_changes`, `detect_dead_code`, `search_code`, `query_graph`, `query_planner`, `delete_project`, `search_codebase`, `query_memory`.

## 6. Desktop GUI

```bash
nexuscontext-gui
```

Requires `nexusd serve` (the systemd unit above) to be running - the GUI is a client of the control socket, not a standalone tool. Seven tabs: Dashboard (status + auto-sync watcher count), Projects (index/reindex/delete), Search, Architecture (node/edge counts, busiest files, language breakdown), Visualize (renders a function's call neighborhood as an image via Graphviz - install `graphviz` for this one; everything else works without it), Config, Logs.

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
enabled = false   # explicit feature switch - filling in endpoint/model below doesn't turn it on
endpoint = "http://localhost:11434/v1"   # OpenAI-compatible; Ollama, LM Studio, vLLM, etc.
model = "nomic-embed-text"
allow_remote = false   # must be true to use a non-loopback/private endpoint

allowed_roots = []   # if non-empty, index_repository/reindex refuses paths outside these
```

Env var overrides: `NEXUS_CACHE_DIR` (data dir), `NEXUS_LOG_LEVEL` (`trace`/`debug`/`info`/`warn`/`error`), `NEXUS_LOG_FORMAT=json` (structured logs, `serve`/`mcp` modes both support it).

All four `[embeddings]` fields can also be set from the GUI's Config tab, not just by hand-editing `config.toml` - including a "Test Connection" button that embeds a short probe string and reports back the model/dimension/latency, so you can verify an endpoint works before enabling it. From the CLI: `nexus test-embeddings` (no `--project` - it's a global config check) and `nexus search-codebase <query> --project <path>`. After enabling and reindexing, `index_repository`'s response includes `embeddings_status` (e.g. `"ok: 342 chunks embedded"`, `"skipped: disabled"`, `"partial: endpoint became unreachable after 96 chunks"`) so you don't need a second round-trip to know whether semantic search will actually work.

## Known limitations (see `README.md` for full detail)

- Semantic search (`search_codebase`, `query_memory`) works, but is off by default and needs a reachable embedding endpoint (`embeddings.enabled = true` plus a real `endpoint`/`model`) - structural tools work fully without any of this. There's no dedicated vector store either (the original proposal's LanceDB pick was never built); embeddings are plain BLOBs in the same SQLite graph.db, ranked by brute-force cosine similarity - fine at this project's actual scale.
- Call resolution is name-based, not import-aware: same-file matches win, and a cross-file call resolves only when the callee name is unique project-wide. Two files defining the same-named function, with no local match in the caller's file, stays unresolved rather than guessing wrong.
- 11 languages supported (Rust, Python, JavaScript, TypeScript/TSX, Go, Java, C, C++, C#, Ruby, PHP), but call-graph quality varies: solid for Rust/Python/JS/TS/Go/Java/Ruby; structural-only (functions/types work, but no call edges) for C/C++/C#/PHP, since those languages' community-maintained tag queries don't capture calls the same way - see `language.rs` for specifics.
- Reindexing is a full rebuild, not an incremental diff (though concurrent rebuilds of the same project are now safe - see above).
- `query_graph`'s Cypher-lite supports exactly one pattern shape (`MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...] RETURN a|b`) - not a real query language. `Kind` can also be `Section` (a markdown heading) alongside `Function`/`Type`/`File`.
- `search_code`'s full-text index covers files tree-sitter parses (any of the 11 supported languages) plus markdown docs (`.md`/`.markdown`, headings extracted into `Section` nodes with `CONTAINS` edges for nesting) - other file types aren't indexed yet.
- The Flatpak manifest (`packaging/flatpak/`) hasn't been built - see its README for the remaining steps.
