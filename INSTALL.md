# Installing and Using NexusContext

This covers what's actually built and working today (Phases 0-7 of the roadmap in `README.md`). It assumes Ubuntu/GNOME.

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
nexus status
```

**Sharing an index with teammates** (skips the first reindex on their end):

```bash
nexus export /path/to/your/project   # writes .nexuscontext/index.db.zst next to source
# ... teammate clones the repo, then:
nexus import /path/to/their/checkout
```

## 5. MCP tools available to agents

Once `nexusd mcp` is wired into an agent, these tools are exposed (no embeddings/network required for any of them except the last two, which are stubbed pending an embedding pipeline):

`index_repository`, `search_graph`, `trace_call_path`, `get_file_context`, `get_architecture`, `detect_changes`, `query_planner`, `search_codebase`, `query_memory`.

## 6. Desktop GUI

```bash
nexuscontext-gui
```

Requires `nexusd serve` (the systemd unit above) to be running - the GUI is a client of the control socket, not a standalone tool. Five tabs: Dashboard, Projects, Search, Config, Logs.

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

## Known limitations (see `README.md` for full detail)

- Semantic search (`search_codebase`, `query_memory`) is not implemented yet - structural tools work fully without it.
- `trace_call_path` only resolves calls within the same file.
- Reindexing is a full rebuild, not an incremental diff.
- The Flatpak manifest (`packaging/flatpak/`) hasn't been built - see its README for the remaining steps.
