# Installing and Using NexusContext

This covers what's actually built and working today (see the full phase-by-phase roadmap in `README.md`). It assumes Ubuntu/GNOME.

## 0. Download a release (Linux/macOS/Windows)

Tagged releases publish real binaries via GitHub Actions - no toolchain needed. Grab the latest from the [Releases page](https://github.com/devNalyx/NexusContext/releases):

- **Linux**: a `.deb` or `.rpm`, or a plain `nexuscontext-linux-<x86_64|arm64>.tar.gz` for other distros - full daemon/CLI/GUI, either architecture.
- **macOS**: `nexuscontext-macos-aarch64.tar.gz` (Apple Silicon) - CLI + full daemon (`nexusd mcp` and `nexusd serve` both work), no native GUI build for macOS. Intel Macs run this fine under Rosetta 2; a native `x86_64` build was attempted but dropped after GitHub's own runner class for it never assigned a runner (a real capacity constraint, not a workflow bug - see `release.yml`). Unsigned binaries need `xattr -d com.apple.quarantine <binary>` or a right-click-Open the first time, since they aren't notarized.
- **Windows**: `nexuscontext-windows-<x86_64|arm64>.zip` - `nexus` CLI + `nexusd mcp` only. `nexusd serve` (the control API, background watcher, GUI target) isn't supported yet - see [issue #16](https://github.com/devNalyx/NexusContext/issues/16). Every MCP tool works fully without it; reindex manually (`nexus reindex`) rather than relying on the background watcher to keep a project warm.

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

**As a background service** (for the GUI/GNOME extension to talk to) - the packaged systemd unit is Linux-only. `nexusd serve` itself also runs on macOS (just start it directly, e.g. `nexusd serve &`, or wire up your own `launchd` agent - no packaged one is shipped yet) but isn't supported on Windows at all yet (see [issue #16](https://github.com/devNalyx/NexusContext/issues/16)):

```bash
systemctl --user enable --now nexuscontext.service
systemctl --user status nexuscontext.service
```

**As an MCP server** (what your IDE/agent should launch, *not* what you run by hand) - works identically on every platform, Windows included: configure your MCP client to run `nexusd mcp` as a subprocess. For Claude Code, add to `.mcp.json`:

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

### Upgrading an existing install

`nexus --version` and `nexusd --version` print the installed version; `nexus status` prints it alongside the version the running daemon is actually serving, with an explicit `RESTART REQUIRED` line when they differ. MCP subprocesses (`nexusd mcp`) started before the upgrade also keep the old binary, so after upgrading:

1. Restart the daemon (`systemctl --user restart nexuscontext.service`, or stop/start your `nexusd serve` process).
2. Restart or reload your IDE / agent session so its `nexusd mcp` subprocess is relaunched.

When replacing binaries manually, **rename a freshly copied file over the old one instead of copying in place** - overwriting an executable that is currently running fails with `Text file busy`:

```bash
cp /path/to/new/nexusd /usr/local/bin/nexusd.new && mv -f /usr/local/bin/nexusd.new /usr/local/bin/nexusd
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
nexus status                                             # installed + running version, restart state, paths
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
nexus install                 # dry run — prints the plan, writes nothing
nexus install --yes           # apply for Claude Code / Claude Desktop
nexus install opencode --yes  # project-local OpenCode V2 plug + MCP + config
nexus install opencode --global --yes  # or: ~/.config/opencode, one install for all projects
nexus install opencode --uninstall --yes  # undo exactly what was installed
nexus install opencode --global --uninstall --yes  # undo the global install
```

Claude Code is registered via its own `claude mcp add` CLI and, with `--yes`, gets a project-local `SessionEnd` hook in `.claude/settings.local.json` plus `.claude/hooks/nexus-session-end.sh`. That hook stages only explicitly signposted lines into the absorbed Myelin pending-review queue; it never stores a raw transcript or promotes an excerpt on its own. To undo a Claude install, remove the `nexuscontext` MCP entry with `claude mcp remove nexuscontext`, delete `.claude/hooks/nexus-session-end.sh`, and remove the matching `SessionEnd` group from `.claude/settings.local.json` (v1 has no `--uninstall` for Claude; OpenCode's manifest-based uninstall is the only automated one). Claude Desktop gets one entry merged into `claude_desktop_config.json` without touching anything else already in it. OpenCode V2 gets `.opencode/plugins/nexuscontext/` (server hook handlers, typed RPC contract, TUI status slot + session panel and its pure formatters) plus one `mcp.servers.nexuscontext` entry in `opencode.json`; with `--global` the plugin goes to `$XDG_CONFIG_HOME/opencode/plugins/nexuscontext` (default `~/.config/opencode`) and the same MCP entry is merged into that config's `opencode.json`. Uninstall restores the pre-install value of that entry (or removes it) and deletes only files from the matching project-local or global install manifest. Prints a generic config snippet for anything else, rather than guessing at a config format it can't verify.

**Curated memory** — agent-staged, human-approved facts with file anchors and freshness (see the wiki's CLI Reference):

```bash
nexus memory add Decision "Title" --body "..." --anchor src/lib.rs --project /path/to/project
nexus memory list --project /path/to/project
nexus memory approve <id> --actor human:you --project /path/to/project
nexus memory recall "query" --project /path/to/project
nexus memory export --project /path/to/project   # .nexus/memory/, OKF v0.3
nexus memory bundles --project /path/to/project --root /path/to/workspace  # routing only
```

## 5. MCP tools available to agents

Once `nexusd mcp` is wired into an agent, tools are exposed through one catalog: 13 structural/graph tools, 11 absorbed Myelin procedural-skill tools, and 6 curated declarative-memory tools. No network calls of any kind.

Structural (13): `index_repository`, `list_sources`, `search_graph`, `trace_call_path`, `get_file_context`, `get_architecture`, `detect_changes`, `detect_dead_code`, `search_code`, `query_graph`, `query_planner`, `get_session_usage`, `delete_project`.

Curated memory (6): `recall`, `list_facts`, `curate_fact`, `review_fact`, `export_memory`, `import_memory`. Only `recall` is advertised by default; curation and review are opt-in. An agent cannot approve its own staging: auto-injection requires a current `human:` sign-off, `status: stable` and a non-stale anchor.

Procedural memory (11): `record_observation`, `list_warmup_queue`, `list_skills`, `promote_skill`, `record_skill_feedback`, `mark_skill_used`, `list_pending_review`, `dismiss_pending_review`, `archive_skill`, `restore_skill`, `render_skill_graph`.

By default only 13 of these 30 are actually advertised to an agent (the `standard` preset - see Section 8's `[tools]` block) to cut the fixed per-session token cost of loading tool schemas. Set `preset = "full"` to get all 30, `preset = "minimal"` for just the 5 core read tools, or list exact tool names via `enabled`.

## 6. Desktop GUI (deprecated — maintenance only)

```bash
nexuscontext-gui
```

**Deprecated:** this GTK4/libadwaita app is Linux/GNOME-only and receives no new features. New interactive work goes into the OpenCode plug's TUI session panel (`/nexus`) and Desktop panel; see [#107](https://github.com/devNalyx/NexusContext/issues/107). It will be removed after those reach parity.

Requires `nexusd serve` (the systemd unit above) to be running - the GUI is a client of the control socket, not a standalone tool. Current source has eight views: Dashboard (status + inotify watch budget), Projects (index/reindex/import/export/delete), Search, Architecture, Visualize (Graphviz PNG), Review (read-only), Usage (lifetime aggregates), Logs (last 300 lines). The Config tab described in older docs no longer exists.

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
allowed_roots = []   # if non-empty, index_repository/reindex refuses paths outside these

[watcher]
warm_window_secs = 21600   # 6h default - a project not queried within this window stops being
                           # auto-watched/auto-reindexed in the background (still catches up with
                           # one synchronous reindex the next time it's actually queried again)

[tools]
preset = "standard"   # "minimal" (5 core read tools) | "standard" (default, 13) | "full" (all 30)
# enabled = ["search_code", "get_architecture"]   # optional explicit list, overrides preset
```

Env var overrides: `NEXUS_CONFIG_DIR` (config dir, i.e. where `config.toml` lives), `NEXUS_CACHE_DIR` (data dir), `NEXUS_LOG_LEVEL` (`trace`/`debug`/`info`/`warn`/`error`), `NEXUS_LOG_FORMAT=json` (structured logs, `serve`/`mcp` modes both support it).

The GUI's Projects tab also has **Import** (top row, next to Index/Reindex - point it at a path with a `.nexuscontext/index.db.zst` artifact, e.g. one a teammate exported and committed) and, per project, **Export** (writes that same artifact into the project so it can be shared) - the same `nexus export`/`nexus import` CLI commands, now reachable without leaving the GUI.

## Known limitations (see `README.md` for full detail)

- Call resolution is name-based, not import-aware: same-file matches win, and a cross-file call resolves only when the callee name is unique project-wide. Two files defining the same-named function, with no local match in the caller's file, stays unresolved rather than guessing wrong.
- 11 languages supported (Rust, Python, JavaScript, TypeScript/TSX, Go, Java, C, C++, C#, Ruby, PHP), but call-graph quality varies: solid for Rust/Python/JS/TS/Go/Java/Ruby; structural-only (functions/types work, but no call edges) for C/C++/C#/PHP, since those languages' community-maintained tag queries don't capture calls the same way - see `language.rs` for specifics.
- Reindexing is a full rebuild, not an incremental diff (though concurrent rebuilds of the same project are now safe - see above).
- A project not queried within `warm_window_secs` (6h default) stops being auto-watched in the background - the first tool call after that gap pays for a synchronous full reindex before returning results, which can take minutes on a large project, rather than the usual near-instant response.
- `query_graph`'s Cypher-lite supports exactly one pattern shape (`MATCH (a:Kind)-[:EDGE]->(b:Kind) [WHERE ...] RETURN a|b`) - not a real query language. `Kind` can also be `Section` (a markdown heading) alongside `Function`/`Type`/`File`.
- `search_code`'s full-text index covers files tree-sitter parses (any of the 11 supported languages) plus markdown docs (`.md`/`.markdown`, headings extracted into `Section` nodes with `CONTAINS` edges for nesting). YAML/JSON (`.yaml`/`.yml`/`.json`) is **opt-in** via `[config_index] enabled = true` — key paths, document numbers and line ranges are searchable, with explicit `parsed`/`partial`/`unsupported` states and `exclude` globs applied before ingestion. No templating language is rendered; `metadata_only` avoids storing raw values. Plain `.txt` and other file types remain unindexed. See [[Configuration]] and [[Known-Limitations]].
- The Flatpak manifest (`packaging/flatpak/`) hasn't been built - see its README for the remaining steps.
