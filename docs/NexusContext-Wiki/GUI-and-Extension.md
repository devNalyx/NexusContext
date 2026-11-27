# GUI and GNOME Extension

Neither is required — the daemon and MCP server work fully headless. These
exist because a CLI is genuinely bad at certain things (browsing search
results, seeing indexing status at a glance) without pulling in a web
dashboard, which this project deliberately avoids.

## OpenCode panels (primary interactive surface)

The OpenCode plug (`integrations/opencode-nexuscontext/`) is the replacement
for the GTK app. All views call one typed RPC contract provided by the
plugin's server part; no view opens the daemon socket itself.

- **TUI:** `/nexus` opens a `session.panel` with tabs for status, sources,
  projects, search, review, architecture, graph, stats and logs. The footer
  shows the live context-ledger estimate. Available in terminal OpenCode.
- **Desktop (experimental):** the `./gui-experimental` entry contributes a
  NexusContext tab beside Context in OpenCode Desktop. It uses the same RPC
  methods and the same pure formatters as the TUI; the GUI SDK is explicitly
  experimental and may change between releases.
- **Install:** `nexus install opencode --global --yes` writes one plugin for all
  projects; without `--global`, the install is project-local. The daemon
  (`nexusd serve`) must still be running for live data.

Registration of knowledge sources stays CLI-only (`nexus source add`); no
panel can promote a fact or approve a source. See the
[plug README](../../integrations/opencode-nexuscontext/README.md).

## NexusContext Manager (`nexuscontext-gui`) — deprecated

**Deprecated, maintenance-only. Do not add new features here.** New
interactive work goes into the OpenCode panels above; the GTK app is scheduled
for removal once the Desktop panel reaches parity. See
[#107](https://github.com/devNalyx/NexusContext/issues/107).

GTK4 + libadwaita, native Linux desktop look, no Electron/web-view
overhead. Talks to the daemon **exclusively** over the control socket
(`nexusd serve` must be running) — never stdio, never MCP. Current views:

- **Dashboard** — daemon status, projects indexed, auto-sync watcher count,
  inotify watch budget/usage (see [[Watcher-and-Freshness]]).
- **Projects** — index/reindex/delete a project, node/edge counts, plus
  Export/Import buttons for the shareable index artifact (see
  [[Storage-and-Data-Model]]).
- **Search** — ad-hoc structural query box with results, for trying
  queries without an agent in the loop.
- **Architecture** — node/edge counts, busiest files, language breakdown.
- **Visualize** — renders a function's call neighborhood as an image via
  Graphviz (`dot`) — a bounded, depth-limited subgraph, not a whole-project
  graph (which turns into an unreadable hairball past a few hundred
  nodes on any real codebase). `graphviz` is a soft recommend, not a hard
  dependency — every other tab works without it.
- **Usage** — the same `stats.get` data the control API exposes,
  per-tool/method call counts, latency, and auto-reindex history.
- **Review** — read-only view of stale facts and skill candidates; no approval
  buttons (approval stays human-side).
- **Logs** — the last 300 daemon log lines, also exposed to panels as a bounded
  `logs.tail` RPC method (max 500 lines, no caller-supplied path).

There is no Config view in current source, despite older documentation.

## GNOME Shell extension

Deliberately thin: a top-bar icon (idle/indexing/error state) and a
dropdown with quick stats plus a launcher for the full GTK4 app. Runs
inside `gnome-shell`'s own process (GJS) — this is exactly why it stays
minimal. Shell extensions that do real work are a common source of Shell
crashes and the most likely part of this stack to break across GNOME
version upgrades; anything heavier belongs in the GTK4 app.

```bash
cp -r extension/nexuscontext@nexuscontext.local ~/.local/share/gnome-shell/extensions/
gnome-extensions enable nexuscontext@nexuscontext.local
```

New extensions require a Shell restart to be picked up (log out/in on
Wayland).

## Related

[[Architecture]] · [[Watcher-and-Freshness]] · [[Storage-and-Data-Model]] ·
[[Security-Model]]
