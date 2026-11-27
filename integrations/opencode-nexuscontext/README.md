# opencode-nexuscontext

OpenCode **V2** plug for NexusContext. One package with four parts:

- **Server** (`./`) — registers the injection/ledger hooks and the typed RPC
  surface over `nexusd`'s local control socket. It owns the daemon connection.
- **TUI** (`./tui`) — a `home.footer.status` ledger readout and a session panel
  with tabs over the RPC contract.
- **RPC** (`./rpc`) — the shared `Rpc.define` contract. Views import this, not
  the socket client, so they cannot invent new daemon calls.
- **Desktop GUI** (`./gui-experimental`) — an experimental OpenCode Desktop
  panel beside Context. Same contract, same pure formatters as the TUI. The GUI
  SDK is explicitly experimental and may change between releases.

There is no LLM and no approval logic in the adapter. The daemon performs
recall, renders the bounded injection, and records the ledger; the harness
reports provider tokens/cost. Estimates stay separate from provider numbers.
Source/fact approval is deliberately not part of this surface — registration
is CLI-only (`nexus source add`).

## Panels and commands

In the TUI, open the panel with `/nexus` (or `/nc`). Arguments switch tabs or
run an explicit project action:

```text
/nexus                                  open the panel (status tab)
/nexus sources | projects | search | review | architecture | graph | stats | logs
/nexus path /abs/repo                   set the repo all tabs operate on
/nexus search <pattern>                 search indexed symbols
/nexus graph <function_name>            show a bounded DOT call neighborhood
/nexus reindex <path>                   reindex a project
/nexus import <path>                    import an exported project
/nexus export <path>                    export a project
/nexus remove <path> --confirm delete   delete a project (explicit confirmation required)
```

In OpenCode Desktop, `/nexus` or the side panel's **+ → NexusContext** item
opens the same view. The Desktop panel has explicit tabs, a query field for
search/graph, and a confirmation field that must contain `delete` before a
project can be removed.

Both panels are read-only except for those four project actions; `removeProject`
rejects a call without the literal `confirm: "delete"`, so an agent cannot
silently delete a project. A daemon that is down renders an explicit error,
never an empty-looking "all clear".

## Install

```bash
nexus install opencode --yes              # project-local (.opencode/plugins/, opencode.json)
nexus install opencode --global --yes     # ~/.config/opencode, one install for every project
nexus install opencode --uninstall --yes
nexus install opencode --global --uninstall --yes
```

The installer embeds the prebuilt Desktop bundle, so there is no `npm install`
step for users. OpenCode Desktop reads its plugin list from the global config,
so use `--global` if you want the Desktop panel on every window. Start
`nexusd serve` before opening OpenCode — the plug never starts the daemon
itself. The control socket is Unix-only; on Windows the panels show an offline
message while MCP tools and the CLI keep working.

## Develop

```bash
npm test             # offline smoke test (stubs SDK + socket, no network)
npm run typecheck:gui
npm run build:gui    # regenerate dist/gui-experimental.cjs after editing src/gui.tsx
```

`dist/gui-experimental.cjs` is committed because OpenCode Desktop needs one
prebuilt CommonJS file and `nexus install` embeds it at compile time. Regenerate
it whenever the GUI source or the shared RPC/formatter modules change.

The smoke test asserts hook ordering, one keyed instruction entry across
prompt/context/compaction, tool/usage telemetry, RPC registration and its
confirmation guard, socket resolution, and the pure panel formatters and error
rendering. No OpenCode install, no network.

## Notes

- `home.footer.status` polls the RPC `ledger` method every 5 s; a missing daemon
  shows `NC · offline` and never breaks a session.
- Project actions use a longer RPC deadline (5 min) because a cold reindex can
  legitimately take minutes; hook-time calls keep the short 2 s deadline.
- The separate `opencode-nexuscontext-statusline` plugin still writes a
  per-session "reads avoided" tally for tmux/ocstatusline; OpenCode has no
  native status-bar hook.
