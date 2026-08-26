# GUI and GNOME Extension

Neither is required — the daemon and MCP server work fully headless. These
exist because a CLI is genuinely bad at certain things (browsing search
results, seeing indexing status at a glance) without pulling in a web
dashboard, which this project deliberately avoids.

## NexusContext Manager (`nexuscontext-gui`)

GTK4 + libadwaita, native Linux desktop look, no Electron/web-view
overhead. Talks to the daemon **exclusively** over the control socket
(`nexusd serve` must be running) — never stdio, never MCP. Six tabs:

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
